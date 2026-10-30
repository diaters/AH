import { http, webSocket } from '@kit.NetworkKit';
import { asset } from '@kit.AssetStoreKit';
import { util } from '@kit.ArkTS';
import { BusinessError } from '@kit.BasicServicesKit';
import { CapabilitiesDto, CommandDto, EventBatchDto, PairDto, ReceiptDto, SnapshotDto } from './Protocol';

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
  private socket?: webSocket.WebSocket;
  private generation: number = 0;

  configure(address: string, instance: string, caPath: string): void {
    this.close();
    if (!/^https:\/\/[a-zA-Z0-9.:[\]-]+$/.test(address) || instance.length !== 36) {
      throw new Error('请填写 HTTPS 地址与完整实例 ID');
    }
    this.address = address;
    this.instance = instance;
    this.caPath = caPath;
  }

  private async request(path: string, method: http.RequestMethod, body: string = ''): Promise<string> {
    const request = http.createHttp();
    const headers: Record<string, string> = { 'Content-Type': 'application/json', 'Authorization': `Bearer ${this.credential}` };
    try {
      const options: http.HttpRequestOptions = {
        method: method, header: headers, connectTimeout: 10000, readTimeout: 15000,
        expectDataType: http.HttpDataType.STRING
      };
      if (body.length > 0) { options.extraData = body; }
      if (this.caPath.length > 0) { options.caPath = this.caPath; }
      const response = await request.request(`${this.address}${path}`, options);
      if (response.responseCode < 200 || response.responseCode >= 300) {
        throw new Error(`HTTP ${response.responseCode}：${response.result as string}`);
      }
      return response.result as string;
    } catch (error) { throw new Error((error as Error).message || 'HTTPS 请求失败'); }
    finally { request.destroy(); }
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
        if (batch.error) { onError(batch.error); } else { onChange(); }
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
