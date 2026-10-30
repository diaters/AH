import { http, webSocket } from '@kit.NetworkKit';
import { asset } from '@kit.AssetStoreKit';
import { util } from '@kit.ArkTS';
import { BusinessError } from '@kit.BasicServicesKit';
import { cryptoFramework } from '@kit.CryptoArchitectureKit';
import { fileIo } from '@kit.CoreFileKit';
import { CapabilitiesDto, ClientFileDto, CommandDto, DevicesDto, DiagnosticsDto, EventBatchDto, PairDto, ReceiptDto, SnapshotDto } from './Protocol';

// 服务端 `--client-pair` 输出的单个配对链接。
export interface PairingLink { instance: string; code: string; fingerprint: string; addresses: string[]; }

export function parseLink(text: string): PairingLink {
  const value = text.trim();
  const prefix = 'harness://pair?';
  if (!value.startsWith(prefix)) { throw new Error('无法识别配对链接，请复制服务端输出的完整链接'); }
  const link: PairingLink = { instance: '', code: '', fingerprint: '', addresses: [] };
  for (const part of value.substring(prefix.length).split('&')) {
    const index = part.indexOf('=');
    if (index <= 0) { continue; }
    const key = part.substring(0, index);
    const item = decodeURIComponent(part.substring(index + 1));
    if (key === 'i') { link.instance = item; }
    else if (key === 'c') { link.code = item; }
    else if (key === 'f') { link.fingerprint = item.toLowerCase(); }
    else if (key === 'a' && /^[a-zA-Z0-9.:[\]-]+$/.test(item)) { link.addresses.push(item); }
  }
  if (link.instance.length !== 36 || link.code.length === 0 || link.addresses.length === 0) {
    throw new Error('配对链接不完整，请重新生成');
  }
  if (link.fingerprint.length > 0 && !/^[0-9a-f]{64}$/.test(link.fingerprint)) { throw new Error('配对链接中的证书指纹无效'); }
  return link;
}

export function sha256Hex(data: Uint8Array): string {
  const digest = cryptoFramework.createMd('SHA256');
  digest.updateSync({ data: data });
  let hex = '';
  for (const byte of digest.digestSync().data) { hex += byte.toString(16).padStart(2, '0'); }
  return hex;
}

// 首次连接时取回实例 CA，只有与配对链接中的指纹一致才写入沙箱并用于后续 TLS 校验。
// 不同信任根使用不同路径，避免原生 TLS 层复用旧路径对应的证书缓存。
export async function trustCa(address: string, fingerprint: string, dir: string): Promise<string> {
  const path = `${dir}/instance-ca-${fingerprint}.pem`;
  if (fileIo.accessSync(path)) { return path; }
  const request = http.createHttp();
  let pem = '';
  try {
    const options: http.HttpRequestOptions = { method: http.RequestMethod.GET, usingCache: false, reuseConnections: false,
      connectTimeout: 4000, readTimeout: 8000, remoteValidation: 'skip', expectDataType: http.HttpDataType.STRING };
    const response = await request.request(`${address}/v1/ca`, options);
    if (response.responseCode !== 200) { throw new Error(`HTTP ${response.responseCode}`); }
    pem = response.result as string;
  } catch (error) { throw new Error((error as Error).message || '无法获取实例证书'); }
  finally { request.destroy(); }
  const body = pem.split('\n').filter((line: string) => !line.startsWith('-----')).join('').trim();
  let der: Uint8Array;
  try { der = new util.Base64Helper().decodeSync(body); } catch (_) { throw new Error('实例证书格式错误'); }
  if (sha256Hex(der) !== fingerprint) { throw new Error('证书指纹不匹配，可能连接到了错误的设备'); }
  const file = fileIo.openSync(path, fileIo.OpenMode.CREATE | fileIo.OpenMode.READ_WRITE | fileIo.OpenMode.TRUNC);
  try { fileIo.writeSync(file.fd, pem); } finally { fileIo.closeSync(file); }
  return path;
}

// 凭证只进入系统 Asset Store；地址、实例 ID 与 CA 文件由应用沙箱保存。
export class CredentialStore {
  private alias(instance: string): Uint8Array { return new util.TextEncoder().encodeInto(`harness:${instance}`); }
  async save(instance: string, token: string): Promise<void> {
    const attributes: asset.AssetMap = new Map();
    attributes.set(asset.Tag.ALIAS, this.alias(instance));
    attributes.set(asset.Tag.SECRET, new util.TextEncoder().encodeInto(token));
    attributes.set(asset.Tag.ACCESSIBILITY, asset.Accessibility.DEVICE_UNLOCKED);
    attributes.set(asset.Tag.CONFLICT_RESOLUTION, asset.ConflictResolution.OVERWRITE);
    try { await asset.add(attributes); } catch (_) { throw new Error('无法保存设备凭证，请重新生成配对码'); }
  }
  async load(instance: string): Promise<string> {
    const query: asset.AssetMap = new Map();
    query.set(asset.Tag.ALIAS, this.alias(instance));
    query.set(asset.Tag.RETURN_TYPE, asset.ReturnType.ALL);
    try {
    const found = await asset.query(query);
    if (found.length === 0) { return ''; }
    return new util.TextDecoder().decodeToString(found[0].get(asset.Tag.SECRET) as Uint8Array);
    } catch (_) { throw new Error('无法读取设备凭证，请解锁设备或重新配对'); }
  }
}

export class Connection {
  address: string = '';
  instance: string = '';
  caPath: string = '';
  credential: string = '';
  capabilities?: CapabilitiesDto;
  // 探测多个候选地址时使用较短超时，避免不可达网卡拖慢连接。
  connectTimeout: number = 10000;
  private socket?: webSocket.WebSocket;
  private generation: number = 0;

  configure(address: string, instance: string, caPath: string): void {
    this.close();
    if (!/^https:\/\/[a-zA-Z0-9.:[\]-]+$/.test(address) || instance.length !== 36) {
      throw new Error('配对信息无效，请重新扫码');
    }
    this.address = address;
    this.instance = instance;
    this.caPath = caPath;
  }

  private async fetch(path: string, method: http.RequestMethod, body: string | ArrayBuffer = '', type: http.HttpDataType = http.HttpDataType.STRING): Promise<http.HttpResponse> {
    const request = http.createHttp();
    const headers: Record<string, string> = { 'Content-Type': typeof body === 'string' ? 'application/json' : 'application/octet-stream', 'Authorization': `Bearer ${this.credential}` };
    try {
      // 身份、权限和快照必须来自当前连接，不能复用上一次信任配置下的响应。
      const options: http.HttpRequestOptions = { method: method, header: headers, usingCache: false, reuseConnections: false, connectTimeout: this.connectTimeout, readTimeout: type === http.HttpDataType.ARRAY_BUFFER ? 180000 : 30000, expectDataType: type };
      // 默认 5 MB 上限会截断产物下载；服务端单个原机产物最大 64 MiB。
      if (type === http.HttpDataType.ARRAY_BUFFER) { options.maxLimit = 100 * 1024 * 1024; }
      if (typeof body !== 'string' || body.length > 0) { options.extraData = body; }
      if (this.caPath.length > 0) { options.caPath = this.caPath; }
      const response = await request.request(`${this.address}${path}`, options);
      if (response.responseCode < 200 || response.responseCode >= 300) {
        throw new Error(`HTTP ${response.responseCode}：${typeof response.result === 'string' ? response.result : '请求失败'}`);
      }
      return response;
    } catch (error) { throw new Error((error as Error).message || 'HTTPS 请求失败'); }
    finally { request.destroy(); }
  }
  private async request(path: string, method: http.RequestMethod, body: string = ''): Promise<string> {
    return (await this.fetch(path, method, body)).result as string;
  }
  async upload(session: string, id: string, name: string, bytes: ArrayBuffer): Promise<ClientFileDto> {
    const response = await this.fetch(`/v1/sessions/${session}/files/${id}?name=${encodeURIComponent(name)}`, http.RequestMethod.PUT, bytes);
    return JSON.parse(response.result as string) as ClientFileDto;
  }
  async download(id: string): Promise<ArrayBuffer> {
    return (await this.fetch(`/v1/files/${id}`, http.RequestMethod.GET, '', http.HttpDataType.ARRAY_BUFFER)).result as ArrayBuffer;
  }

  async pair(code: string, deviceName: string): Promise<void> {
    const body: Record<string, string> = { 'instance_id': this.instance, 'code': code, 'device_name': deviceName };
    const paired = JSON.parse(await this.request('/v1/pair', http.RequestMethod.POST, JSON.stringify(body))) as PairDto;
    if (paired.instance_id !== this.instance) { throw new Error('实例身份不匹配'); }
    try { await new CredentialStore().save(this.instance, paired.token); } catch (_) { throw new Error('无法保存设备凭证，请重新配对'); }
    this.credential = paired.token;
    await this.verify();
  }

  async checkIdentity(): Promise<void> {
    // 在把设备凭证发往新地址之前核对实例身份。
    this.credential = '';
    const identity = JSON.parse(await this.request('/v1/identity', http.RequestMethod.GET)) as CapabilitiesDto;
    if (identity.instance_id !== this.instance || identity.protocol_version !== 1) { throw new Error('实例身份或协议版本不匹配'); }
  }

  async verify(): Promise<void> {
    const caps = JSON.parse(await this.request('/v1/capabilities', http.RequestMethod.GET)) as CapabilitiesDto;
    if (caps.instance_id !== this.instance || caps.protocol_version !== 1) { throw new Error('实例身份或协议版本不匹配'); }
    this.capabilities = caps;
  }

  async snapshot(): Promise<SnapshotDto> {
    const snapshot = JSON.parse(await this.request('/v1/snapshot', http.RequestMethod.GET)) as SnapshotDto;
    if (snapshot.instance_id !== this.instance) { throw new Error('快照实例身份不匹配'); }
    return snapshot;
  }
  async command(command: CommandDto): Promise<ReceiptDto> {
    return JSON.parse(await this.request('/v1/commands', http.RequestMethod.POST, JSON.stringify(command))) as ReceiptDto;
  }
  async status(id: string): Promise<ReceiptDto> {
    return JSON.parse(await this.request(`/v1/commands/${id}`, http.RequestMethod.GET)) as ReceiptDto;
  }
  async devices(): Promise<DevicesDto> {
    return JSON.parse(await this.request('/v1/devices', http.RequestMethod.GET)) as DevicesDto;
  }
  async revokeDevice(id: string): Promise<void> {
    await this.request(`/v1/devices/${id}`, http.RequestMethod.DELETE);
  }
  async diagnostics(): Promise<DiagnosticsDto> {
    return JSON.parse(await this.request('/v1/diagnostics', http.RequestMethod.GET)) as DiagnosticsDto;
  }
  async acknowledgeNotification(id: string, read: boolean): Promise<void> {
    const body: Record<string, boolean> = { 'read': read };
    await this.request(`/v1/notifications/${id}/ack`, http.RequestMethod.POST, JSON.stringify(body));
  }
  async retryNotification(id: string, allowUnknown: boolean): Promise<void> {
    const body: Record<string, boolean> = { 'allow_unknown': allowUnknown };
    await this.request(`/v1/notifications/${id}/retry`, http.RequestMethod.POST, JSON.stringify(body));
  }
  supports(operation: string): boolean { return this.capabilities?.operations.includes(operation) ?? false; }

  async subscribe(cursor: number, onChange: () => void, onError: (text: string) => void): Promise<void> {
    this.close();
    const generation = this.generation;
    let socket: webSocket.WebSocket;
    try { socket = webSocket.createWebSocket(); } catch (_) { throw new Error('无法建立实时连接'); }
    this.socket = socket;
    socket.on('message', (error: BusinessError, value: string | ArrayBuffer) => {
      if (generation !== this.generation || error || typeof value !== 'string') { return; }
      try {
        const batch = JSON.parse(value) as EventBatchDto;
        if (batch.error) { this.close(); onError(batch.error); } else { onChange(); }
      } catch (_) { onError('同步数据格式错误'); }
    });
    socket.on('error', () => { if (generation === this.generation) { onError('连接中断，请重连补齐状态'); } });
    socket.on('close', () => { if (generation === this.generation) { onError('连接已关闭，请重连'); } });
    const headers: Record<string, string> = { 'Authorization': `Bearer ${this.credential}` };
    const options: webSocket.WebSocketRequestOptions = { header: headers };
    if (this.caPath.length > 0) { options.caPath = this.caPath; }
    try {
      await socket.connect(`${this.address.replace('https:', 'wss:')}/v1/stream?after=${cursor}`, options);
    } catch (_) { throw new Error('实时连接失败，请检查证书或重新连接'); }
  }
  close(): void {
    this.generation++;
    if (this.socket) { this.socket.close().catch(() => {}); this.socket = undefined; }
  }
}
