import { common } from '@kit.AbilityKit';
import { preferences } from '@kit.ArkData';
import { fileIo, fileUri, picker } from '@kit.CoreFileKit';
import { util } from '@kit.ArkTS';
import { image } from '@kit.ImageKit';
import { Connection, CredentialStore } from '../services/Connection';
import { ApprovalDto, ApprovalOptionDto, AutomationDto, ClientFileDto, CommandDto, DeviceDto, DiagnosticsDto, MessageDto, NotificationDto, ObservationDto, PendingUploadDto, ProgressDto, QuestionDto, RunDto, ScheduleActionDto, ScheduleDto, ScheduleInputDto, SessionDto, TaskDto, TodoDto } from '../services/Protocol';

@Entry
@Component
struct Index {
  @State address: string = '';
  @State instance: string = '';
  @State pairingCode: string = '';
  @State caPem: string = '';
  @State status: string = '连接你的 Harness 实例';
  @State sessions: SessionDto[] = [];
  @State selected: string = '';
  @State draft: string = '';
  @State windowWidth: number = 390;
  @State settings: boolean = true;
  @State connected: boolean = false;
  @State busy: boolean = false;
  @State hasPending: boolean = false;
  @State questionDrafts: Record<string, string> = {};
  @State view: string = 'sessions';
  @State automation: AutomationDto = { available: false, schedules: [], timezone: '' };
  @State planName: string = '';
  @State planContent: string = '';
  @State planExpression: string = 'cron:0 8 * * *';
  @State planEnabled: boolean = true;
  @State planNotify: string = 'reply';
  @State editPlanId: string = '';
  @State notifications: NotificationDto[] = [];
  @State notificationsTruncated: boolean = false;
  @State notificationOverflow: number = 0;
  @State files: ClientFileDto[] = [];
  @State attachments: string[] = [];
  @State pendingUpload: PendingUploadDto | undefined = undefined;
  @State workspaceTab: string = 'chat';
  @State devices: DeviceDto[] = [];
  @State diagnostics: DiagnosticsDto | undefined = undefined;
  @State preview: image.PixelMap | undefined = undefined;
  @State sessionSearch: string = '';
  @State approvalFeedback: Record<string, string> = {};
  private conversationScroller: Scroller = new Scroller();
  private editPlanRevision: string = '';
  @StorageLink('harnessForeground') @Watch('lifecycle') foreground: boolean = true;
  private connection: Connection = new Connection();
  private storage?: preferences.Preferences;
  private pending?: CommandDto;
  private lastCommandId: string = '';
  private refreshing: boolean = false;
  private refreshAgain: boolean = false;
  private reconnectTimer: number = -1;
  private reconnectDelay: number = 1000;

  aboutToAppear(): void {
    try {
    const context = this.getUIContext().getHostContext() as common.UIAbilityContext;
    this.storage = preferences.getPreferencesSync(context, { name: 'harness' });
    this.address = this.storage.getSync('address', '') as string;
    this.instance = this.storage.getSync('instance', '') as string;
    this.caPem = this.storage.getSync('ca', '') as string;
    this.selected = this.storage.getSync('selected', '') as string;
    this.draft = this.storage.getSync(`draft:${this.selected}`, '') as string;
    this.attachments = JSON.parse(this.storage.getSync(`attachments:${this.selected}`, '[]') as string) as string[];
    const upload = this.storage.getSync('pending-upload', '') as string;
    if (upload) { this.pendingUpload = JSON.parse(upload) as PendingUploadDto; }
    const saved = this.storage.getSync('pending', '') as string;
    if (saved.length > 0) {
      try { this.pending = JSON.parse(saved) as CommandDto; this.hasPending = true; } catch (_) { this.status = '待发送记录损坏，请检查'; }
    }
    } catch (_) { this.status = '无法读取本地状态，请检查应用存储'; }
  }
  aboutToDisappear(): void { this.cancelReconnect(); this.connection.close(); }
  private cancelReconnect(): void { if (this.reconnectTimer >= 0) { clearTimeout(this.reconnectTimer); this.reconnectTimer = -1; } }
  private scheduleReconnect(): void {
    if (this.settings || !this.foreground || this.reconnectTimer >= 0) { return; }
    this.reconnectTimer = setTimeout(() => { this.reconnectTimer = -1; this.connect(false); }, this.reconnectDelay);
    this.reconnectDelay = Math.min(this.reconnectDelay * 2, 30000);
  }
  private lifecycle(): void {
    if (!this.foreground) { this.cancelReconnect(); this.connection.close(); this.connected = false; }
    else if (!this.settings && this.instance.length > 0) { this.connect(false); }
  }
  private save(key: string, value: string): boolean {
    try {
      if (!this.storage) { throw new Error('storage unavailable'); }
      this.storage.putSync(key, value); this.storage.flushSync(); return true;
    } catch (_) { this.status = '无法保存本地状态，尚未发送新命令'; return false; }
  }
  private select(id: string): void {
    this.preview?.release().catch(() => {}); this.preview = undefined;
    this.selected = id; this.save('selected', id);
    try { this.draft = this.storage?.getSync(`draft:${id}`, '') as string ?? ''; }
    catch (_) { this.status = '无法读取草稿'; this.draft = ''; }
    try { this.attachments = JSON.parse(this.storage?.getSync(`attachments:${id}`, '[]') as string ?? '[]') as string[]; }
    catch (_) { this.attachments = []; }
  }
  private current(): SessionDto | undefined { return this.sessions.find((session: SessionDto) => session.id === this.selected); }
  private tasks(): TaskDto[] { const session = this.current(); return session ? Object.values(session.tasks) : []; }
  private approvals(): ApprovalDto[] { const session = this.current(); return session ? Object.values(session.approvals) : []; }
  private questions(): QuestionDto[] { const session = this.current(); return session ? Object.values(session.questions ?? {}) : []; }
  private questionKey(question: QuestionDto): string { return `answer:${this.selected}:${question.task_id}/${question.question_id}`; }
  private questionDraft(question: QuestionDto): string {
    const key = this.questionKey(question);
    if (this.questionDrafts[key] !== undefined) { return this.questionDrafts[key]; }
    try { return this.storage?.getSync(key, '') as string ?? ''; } catch (_) { return ''; }
  }
  private editAnswer(question: QuestionDto, value: string): void {
    const key = this.questionKey(question);
    const next: Record<string, string> = {};
    for (const existing of Object.keys(this.questionDrafts)) { next[existing] = this.questionDrafts[existing]; }
    next[key] = value; this.questionDrafts = next; this.save(key, value);
  }
  private control(task: TaskDto, operation: string): void {
    const command: CommandDto = {operation: operation, command_id: util.generateRandomUUID(), session_id: this.selected,
      task_id: task.task_id, expected_revision: task.revision};
    this.submit(command);
  }
  private answerQuestion(question: QuestionDto): void {
    const command: CommandDto = {operation: 'answer_question', command_id: util.generateRandomUUID(), session_id: this.selected,
      task_id: question.task_id, question_id: question.question_id, answer: this.questionDraft(question)};
    this.submit(command);
  }

  private async connect(pair: boolean): Promise<void> {
    if (this.busy) { return; }
    this.busy = true;
    this.cancelReconnect();
    this.connected = false;
    try {
      if (this.hasPending && this.storage?.getSync('pending-instance', '') !== this.instance.trim()) {
        throw new Error('另一个实例仍有待发送命令，请先处理或清除本地重试');
      }
      const context = this.getUIContext().getHostContext() as common.UIAbilityContext;
      let caPath = '';
      if (this.caPem.trim().length > 0) {
        if (!this.caPem.includes('-----BEGIN CERTIFICATE-----')) { throw new Error('请粘贴 PEM 格式的可信 CA 证书'); }
        caPath = `${context.filesDir}/instance-ca.pem`;
        const file = fileIo.openSync(caPath, fileIo.OpenMode.CREATE | fileIo.OpenMode.READ_WRITE | fileIo.OpenMode.TRUNC);
        try { fileIo.writeSync(file.fd, this.caPem); } finally { fileIo.closeSync(file); }
      }
      this.connection.configure(this.address.trim(), this.instance.trim(), caPath);
      await this.connection.checkIdentity();
      if (pair) {
        await this.connection.pair(this.pairingCode.trim(), 'HarmonyOS 工作台');
        this.pairingCode = '';
      } else {
        this.connection.credential = await new CredentialStore().load(this.instance.trim());
        await this.connection.verify();
      }
      this.save('address', this.address.trim()); this.save('instance', this.instance.trim()); this.save('ca', this.caPem);
      const snapshot = await this.connection.snapshot();
      if (!this.foreground) { this.connection.close(); return; }
      this.sessions = snapshot.sessions;
      this.automation = snapshot.automation ?? { available: false, schedules: [], timezone: '' };
      this.notifications = snapshot.notifications ?? []; this.notificationsTruncated = snapshot.notifications_truncated ?? false; this.notificationOverflow = snapshot.notification_overflow ?? 0;
      this.files = snapshot.files ?? [];
      this.connected = true; this.settings = false;
      this.status = `已连接 · ${this.connection.capabilities?.instance_name ?? 'Harness'}`;
      this.reconnectDelay = 1000;
      await this.connection.subscribe(snapshot.cursor, () => { this.refresh(); }, (text: string) => {
        this.status = text; this.connected = false;
        if (text !== 'unauthorized') { this.scheduleReconnect(); }
      });
    } catch (error) {
      this.connection.close(); this.connected = false;
      this.status = (error as Error).message || '连接失败，请检查地址、证书和凭证';
      if (!this.status.includes('HTTP 401')) { this.scheduleReconnect(); }
    } finally { this.busy = false; }
  }

  private async refresh(): Promise<void> {
    if (this.refreshing) { this.refreshAgain = true; return; }
    this.refreshing = true;
    try {
      do {
        this.refreshAgain = false;
        const snapshot = await this.connection.snapshot();
        this.sessions = snapshot.sessions; this.automation = snapshot.automation ?? { available: false, schedules: [], timezone: '' };
        this.notifications = snapshot.notifications ?? []; this.notificationsTruncated = snapshot.notifications_truncated ?? false; this.notificationOverflow = snapshot.notification_overflow ?? 0;
        this.files = snapshot.files ?? [];
        if (this.lastCommandId.length > 0) {
          const receipt = await this.connection.status(this.lastCommandId);
          if (receipt.status !== 'queued') { this.status = `命令状态：${receipt.status} ${receipt.result_code ?? ''}`; }
        }
      } while (this.refreshAgain);
    } catch (error) { this.status = (error as Error).message; }
    finally { this.refreshing = false; }
  }

  private async submit(command: CommandDto): Promise<void> {
    if (this.busy || !this.connected) { return; }
    this.busy = true;
    this.pending = command; this.hasPending = true;
    if (!this.save('pending-instance', this.instance.trim()) || !this.save('pending', JSON.stringify(command))) { this.busy = false; return; }
    try {
      const receipt = await this.connection.command(command);
      this.lastCommandId = command.command_id;
      this.status = receipt.status === 'queued' ? '输入已入队，尚不代表 Agent 已消费' : `处理状态：${receipt.status}`;
      if (command.operation === 'create_session') { this.select(receipt.session_id); }
      if (command.operation === 'send_text' && receipt.status === 'queued') {
        this.save(`draft:${command.session_id ?? ''}`, '');
        if (command.session_id === this.selected) { this.draft = ''; }
        this.save(`attachments:${command.session_id ?? ''}`, '[]');
        if (command.session_id === this.selected) { this.attachments = []; }
      }
      this.pending = undefined; this.hasPending = false; this.save('pending', '');
      await this.refresh();
    } catch (error) { this.status = `${(error as Error).message}。保留原命令，可查询状态或重试`; }
    finally { this.busy = false; }
  }

  private async queryPending(): Promise<void> {
    if (!this.pending) { return; }
    try { const receipt = await this.connection.status(this.pending.command_id); this.status = `服务器命令状态：${receipt.status} ${receipt.result_code ?? ''}`; }
    catch (error) { this.status = (error as Error).message; }
  }
  private send(): void {
    if ((!this.draft.trim() && this.attachments.length === 0) || this.hasPending) { return; }
    const command: CommandDto = { operation: 'send_text', command_id: util.generateRandomUUID(), session_id: this.selected, content: this.draft, attachment_ids: this.attachments };
    this.submit(command);
  }
  private attachFile(file: ClientFileDto): void {
    if (this.attachments.includes(file.id)) { return; }
    if (this.attachments.length >= 8) { this.status = '一条消息最多附加 8 个文件'; return; }
    this.attachments = this.attachments.concat([file.id]);
    this.save(`attachments:${this.selected}`, JSON.stringify(this.attachments));
  }
  private clearUpload(): void {
    const pending = this.pendingUpload;
    if (!pending || this.busy) { return; }
    if (!this.save('pending-upload', '')) { return; }
    this.pendingUpload = undefined;
    try { fileIo.unlinkSync(pending.local_path); }
    catch (_) { this.status = '已清除本地重试，缓存文件清理失败；服务器文件未删除'; }
  }
  private async uploadFile(retry: boolean): Promise<void> {
    if (this.busy || !this.selected) { return; } this.busy = true;
    try {
      const context = this.getUIContext().getHostContext() as common.UIAbilityContext;
      if (!retry) {
        const options: picker.DocumentSelectOptions = { maxSelectNumber: 1 };
        const selected = await new picker.DocumentViewPicker(context).select(options);
        if (selected.length === 0) { return; }
        const source = fileIo.openSync(selected[0], fileIo.OpenMode.READ_ONLY);
        let bytes: ArrayBuffer;
        try {
          const size = fileIo.statSync(source.fd).size;
          if (size > 10 * 1024 * 1024) { throw new Error('文件不能超过 10 MiB'); }
          bytes = new ArrayBuffer(size);
          if (fileIo.readSync(source.fd, bytes) !== size) { throw new Error('读取文件不完整，请重新选择'); }
        } finally { fileIo.closeSync(source); }
        const id = util.generateRandomUUID();
        const local = `${context.filesDir}/pending-upload-${id}`;
        const cache = fileIo.openSync(local, fileIo.OpenMode.CREATE | fileIo.OpenMode.READ_WRITE | fileIo.OpenMode.TRUNC);
        try { if (fileIo.writeSync(cache.fd, bytes) !== bytes.byteLength) { throw new Error('本地缓存写入不完整'); } }
        finally { fileIo.closeSync(cache); }
        this.pendingUpload = { instance_id: this.instance.trim(), id: id, session_id: this.selected, name: new fileUri.FileUri(selected[0]).name || 'attachment.bin', local_path: local };
        if (!this.save('pending-upload', JSON.stringify(this.pendingUpload))) { return; }
      }
      const pending = this.pendingUpload;
      if (!pending) { return; }
      if (pending.instance_id !== this.instance.trim()) { throw new Error('待上传文件属于其他实例，请切回原实例'); }
      const source = fileIo.openSync(pending.local_path, fileIo.OpenMode.READ_ONLY);
      let bytes: ArrayBuffer;
      try {
        const size = fileIo.statSync(source.fd).size;
        if (size > 10 * 1024 * 1024) { throw new Error('文件过大'); }
        bytes = new ArrayBuffer(size);
        if (fileIo.readSync(source.fd, bytes) !== size) { throw new Error('缓存读取不完整'); }
      } finally { fileIo.closeSync(source); }
      const file = await this.connection.upload(pending.session_id, pending.id, pending.name, bytes);
      if (this.selected === pending.session_id) { this.attachFile(file); }
      this.pendingUpload = undefined; this.save('pending-upload', '');
      fileIo.unlinkSync(pending.local_path);
      this.status = '文件已上传，点击发送后才交给 Agent'; await this.refresh();
    } catch (error) { this.status = `${(error as Error).message}。已保留上传缓存，可重试原文件`; }
    finally { this.busy = false; if (!this.connected) { this.scheduleReconnect(); } }
  }
  private async previewFile(file: ClientFileDto): Promise<void> {
    if (this.busy) { return; } this.busy = true;
    try {
      const source = image.createImageSource(await this.connection.download(file.id));
      try {
        const info = await source.getImageInfo();
        const scale = Math.min(1, 1024 / Math.max(info.size.width, info.size.height));
        const decoded = await source.createPixelMap({ desiredSize: { width: Math.max(1, Math.floor(info.size.width * scale)), height: Math.max(1, Math.floor(info.size.height * scale)) } });
        this.preview?.release().catch(() => {}); this.preview = decoded;
      } finally { await source.release(); }
    } catch (error) { this.status = `无法预览：${(error as Error).message}`; }
    finally { this.busy = false; }
  }
  private async downloadFile(file: ClientFileDto): Promise<void> {
    if (this.busy) { return; } this.busy = true;
    try {
      const context = this.getUIContext().getHostContext() as common.UIAbilityContext;
      const options: picker.DocumentSaveOptions = { newFileNames: [file.name] };
      const targets = await new picker.DocumentViewPicker(context).save(options);
      if (targets.length === 0) { return; }
      const bytes = await this.connection.download(file.id);
      if (bytes.byteLength !== file.size) { throw new Error('下载长度不匹配，尚未写入'); }
      const output = fileIo.openSync(targets[0], fileIo.OpenMode.READ_WRITE | fileIo.OpenMode.TRUNC);
      try { if (fileIo.writeSync(output.fd, bytes) !== bytes.byteLength) { throw new Error('文件写入不完整'); } }
      finally { fileIo.closeSync(output); }
      this.status = '文件已保存';
    } catch (error) { this.status = (error as Error).message; }
    finally { this.busy = false; if (!this.connected) { this.scheduleReconnect(); } }
  }

  @Builder private artifactsView() {
    Scroll() {
      Column({ space: 12 }) {
        Text('附件与产物').fontSize(22).fontWeight(FontWeight.Bold)
        if (this.preview) { Image(this.preview).width('100%').height(260).objectFit(ImageFit.Contain) }
        Button('选择并上传文件').enabled(this.connected && !this.busy && !this.pendingUpload && this.connection.supports('files.transfer')).onClick(() => { this.uploadFile(false); })
        if (this.pendingUpload) {
          Text('上次上传尚未确认。清除本地重试不会删除服务器可能已保存的文件。').fontSize(13)
          Row({space:8}) {
            Button('重试上次上传').enabled(this.connected && !this.busy).onClick(() => { this.uploadFile(true); })
            Button('清除本地上传缓存').enabled(!this.busy).onClick(() => { this.clearUpload(); })
          }
        }
        ForEach(this.files.filter((file: ClientFileDto) => file.session_id === this.selected), (file: ClientFileDto) => {
          Column({ space: 8 }) {
            Text(file.name).fontWeight(FontWeight.Bold)
            Text(`${file.size} 字节 · ${file.source === 'upload' ? '上传附件' : 'Agent 产物'}`).fontSize(12)
            Text(`SHA-256 ${file.sha256}`).fontSize(11).copyOption(CopyOptions.InApp)
            Row({ space: 8 }) {
              if (file.kind === 'image') { Button('预览').enabled(this.connected && !this.busy).onClick(() => { this.previewFile(file); }) }
              Button('保存文件').enabled(this.connected && !this.busy).onClick(() => { this.downloadFile(file); })
              Button('附加到消息').enabled(!this.hasPending && !this.busy).onClick(() => { this.attachFile(file); this.workspaceTab = 'chat'; })
            }
          }.padding(12).width('100%').alignItems(HorizontalAlign.Start).backgroundColor('#FFFFFF').borderRadius(12)
        }, (file: ClientFileDto) => file.id)
      }.width('100%')
    }.width('100%').layoutWeight(1)
  }
  @Builder private executionView() {
    Scroll() {
      Column({ space: 10 }) {
        Text('执行记录（最近 200 条）').fontSize(20).fontWeight(FontWeight.Bold)
        if (this.current()?.observations_truncated) { Text('更早的执行记录已超出保留范围。') }
        ForEach(this.current()?.observations ?? [], (item: ObservationDto) => {
          Column({ space: 6 }) {
            Text(`${item.agent_name} · ${item.tool_name}`).fontWeight(FontWeight.Bold)
            Text(item.kind === 'permission_audit' ? `权限：${item.action ?? ''} · ${item.context ?? ''}` : item.kind === 'tool_started' ? '开始调用' : item.success ? '工具返回成功' : '工具返回错误')
            if (item.input_summary) { Text(item.input_summary).fontSize(12) }
            if (item.output) { Text(item.output).fontSize(12).copyOption(CopyOptions.InApp) }
            if (item.truncated) { Text('输出已截断').fontColor('#956600') }
            if (item.late_result) { Text('任务结束后的返回，不改变任务终态').fontColor('#956600') }
          }.padding(12).width('100%').alignItems(HorizontalAlign.Start).backgroundColor('#FFFFFF').borderRadius(12)
        })
      }.width('100%')
    }.width('100%').layoutWeight(1)
  }
  private feedback(request: ApprovalDto): string {
    const key = `approval:${this.selected}:${request.request_id}`;
    if (this.approvalFeedback[key] !== undefined) { return this.approvalFeedback[key]; }
    try { return this.storage?.getSync(key, '') as string ?? ''; } catch (_) { return ''; }
  }
  private editFeedback(request: ApprovalDto, value: string): void {
    const key = `approval:${this.selected}:${request.request_id}`;
    const next: Record<string, string> = {};
    for (const existing of Object.keys(this.approvalFeedback)) { next[existing] = this.approvalFeedback[existing]; }
    next[key] = value; this.approvalFeedback = next; this.save(key, value);
  }
  private async loadManagement(): Promise<void> {
    if (!this.connected || this.busy) { return; } this.busy = true;
    try { this.devices = (await this.connection.devices()).devices; this.diagnostics = this.connection.supports('diagnostics.read') ? await this.connection.diagnostics() : undefined; }
    catch (error) { this.status = (error as Error).message; }
    finally { this.busy = false; }
  }
  private async revokeDevice(device: DeviceDto): Promise<void> {
    if (this.busy) { return; } this.busy = true;
    try { await this.connection.revokeDevice(device.id); this.devices = (await this.connection.devices()).devices; this.status = '设备凭证已撤销'; }
    catch (error) { this.status = (error as Error).message; }
    finally { this.busy = false; }
  }
  @Builder private managementView() {
    Scroll() {
      Column({ space: 14 }) {
        Row() {
          Text('设备与诊断').fontSize(24).fontWeight(FontWeight.Bold).layoutWeight(1)
          Button('刷新').enabled(this.connected && !this.busy).onClick(() => { this.loadManagement(); })
        }
        if (this.diagnostics) {
          Text(`实例 ${this.diagnostics.instance_id}`).fontSize(13).copyOption(CopyOptions.InApp)
          Text(`Harness ${this.diagnostics.version} · 存储${this.diagnostics.storage_available ? '可用' : '不可用'}`)
          Text(`同步游标 ${this.diagnostics.cursor} · 保留 ${this.diagnostics.event_retention} 条事件`).fontSize(13)
          Text(`${this.diagnostics.sessions} 个会话 · ${this.diagnostics.pending_approvals} 项审批 · ${this.diagnostics.pending_questions} 个问题`)
          Text(`重启后结果未知的命令：${this.diagnostics.unknown_commands}`).fontSize(13)
        }
        Text('当前用户的配对设备').fontSize(20).fontWeight(FontWeight.Bold)
        Text('撤销当前设备后会断开连接，需要重新配对。').fontSize(13)
        ForEach(this.devices, (device: DeviceDto) => {
          Column({space:8}) {
            Text(device.name).fontWeight(FontWeight.Bold)
            Text(device.id).fontSize(12).copyOption(CopyOptions.InApp)
            Text(device.revoked ? '已撤销' : `有效至 ${new Date(device.expires_at * 1000).toLocaleString()}`).fontSize(13)
            Button('撤销凭证').enabled(this.connected && !this.busy && !device.revoked && this.connection.supports('devices.revoke')).onClick(() => { this.revokeDevice(device); })
          }.padding(14).width('100%').alignItems(HorizontalAlign.Start).backgroundColor('#FFFFFF').borderRadius(12)
        }, (device: DeviceDto) => JSON.stringify(device))
        Text('模型配置、全局 Skills 和共享记忆尚未通过此接口开放。').fontSize(13).fontColor('#526070')
      }.width('100%').padding(20).constraintSize({ maxWidth: 850 })
    }.width('100%').layoutWeight(1)
  }
  private approve(request: ApprovalDto, option: string): void {
    const command: CommandDto = { operation: 'respond_approval', command_id: util.generateRandomUUID(), session_id: this.selected,
      request_id: request.request_id, expected_version: request.version, option_id: option, feedback: this.feedback(request) };
    this.submit(command);
  }

  private manageSchedule(action: ScheduleActionDto): void {
    if (!this.selected && this.sessions.length > 0) { this.select(this.sessions[0].id); }
    if (!this.selected) { this.status = '请先创建一个会话，用于接收计划运行结果'; return; }
    const command: CommandDto = { operation: 'schedule', command_id: util.generateRandomUUID(), session_id: this.selected, action: action };
    this.submit(command);
  }
  private notificationState(value: string): string {
    if (value === 'accepted') { return 'MeoW 已接受，设备送达未确认'; }
    if (value === 'sending') { return '正在投递'; }
    if (value === 'unknown') { return '结果未知，可能已发出'; }
    if (value === 'quota_wait') { return '当天额度已用完，等待下个日界线'; }
    if (value === 'failed') { return '投递失败'; }
    return '等待投递';
  }
  private async notificationAction(notification: NotificationDto, retry: boolean): Promise<void> {
    if (this.busy) { return; } this.busy = true;
    try {
      if (retry) { await this.connection.retryNotification(notification.id, notification.state === 'unknown'); }
      else { await this.connection.acknowledgeNotification(notification.id, true); }
      await this.refresh();
    } catch (error) { this.status = (error as Error).message; }
    finally { this.busy = false; }
  }
  @Builder private inboxView() {
    Scroll() {
      Column({ space: 14 }) {
        Text('收件箱').fontSize(24).fontWeight(FontWeight.Bold)
        Text(this.connection.capabilities?.meow_enabled ? '重要通知由服务端 MeoW 投递。接受、设备收到和已读分别记录。' : '此用户未配置 MeoW 通知。待审批和问题仍可在会话中处理。').fontColor('#526070')
        if (this.notificationOverflow > 0) { Text(`通知队列容量不足：${this.notificationOverflow} 条未入队`).fontColor('#B64040') }
        if (this.notificationsTruncated) { Text('这里只展示最近 200 条通知，较早记录仍保留在服务端。') }
        ForEach(this.notifications, (notification: NotificationDto) => {
          Column({ space: 8 }) {
            Text(notification.title).fontSize(18).fontWeight(FontWeight.Bold)
            Text(notification.message).fontSize(14)
            Text(this.notificationState(notification.state)).fontSize(14)
            if (notification.result_code) { Text(notification.result_code).fontSize(12).fontColor('#526070') }
            Text(notification.read_at ? '已读' : notification.received_at ? '客户端已确认同步' : '客户端尚未确认').fontSize(12)
            Row({ space: 8 }) {
              Button('查看会话').onClick(() => { this.select(notification.session_id); this.view = 'sessions'; })
              Button('标记已读').enabled(this.connected && !this.busy && !notification.read_at).onClick(() => { this.notificationAction(notification, false); })
              if (notification.state === 'failed' || notification.state === 'unknown') {
                Button(notification.state === 'unknown' ? '重试（可能重复）' : '重试').enabled(this.connected && !this.busy).onClick(() => { this.notificationAction(notification, true); })
              }
            }
          }.alignItems(HorizontalAlign.Start).width('100%').padding(16).backgroundColor('#FFFFFF').borderRadius(12)
        }, (notification: NotificationDto) => JSON.stringify(notification))
        if (this.notifications.length === 0) { Text('当前没有通知记录') }
      }.padding(20).width('100%').constraintSize({ maxWidth: 850 })
    }.width('100%').layoutWeight(1)
  }
  private editPlan(plan: ScheduleDto): void {
    this.editPlanId = plan.id; this.editPlanRevision = plan.revision;
    this.planName = plan.name; this.planContent = plan.content; this.planExpression = plan.schedule;
    this.planEnabled = plan.enabled; this.planNotify = plan.notify;
  }
  private resetPlan(): void {
    this.editPlanId = ''; this.editPlanRevision = ''; this.planName = ''; this.planContent = '';
    this.planExpression = 'cron:0 8 * * *'; this.planEnabled = true; this.planNotify = 'reply';
  }
  private savePlan(): void {
    const input: ScheduleInputDto = { name: this.planName, content: this.planContent, schedule: this.planExpression, enabled: this.planEnabled, notify: this.planNotify };
    const action: ScheduleActionDto = this.editPlanId.length > 0 ?
      { kind: 'update', schedule_id: this.editPlanId, expected_revision: this.editPlanRevision, input: input } : { kind: 'create', input: input };
    this.manageSchedule(action);
  }
  private togglePlan(plan: ScheduleDto): void {
    const input: ScheduleInputDto = { name: plan.name, content: plan.content, schedule: plan.schedule, enabled: !plan.enabled, notify: plan.notify };
    this.manageSchedule({ kind: 'update', schedule_id: plan.id, expected_revision: plan.revision, input: input });
  }

  @Builder private automationView() {
    Scroll() {
      Column({ space: 14 }) {
        Row({ space: 12 }) {
          Text('自动化').fontSize(24).fontWeight(FontWeight.Bold).layoutWeight(1)
          Button('刷新').enabled(this.connected && !this.busy && !this.hasPending).onClick(() => { this.manageSchedule({ kind: 'list' }); })
        }
        Text(`时区：${this.automation.timezone || '尚未读取'}`).fontColor('#526070')
        if (this.automation.stale) { Text('当前显示缓存，请刷新后操作。').fontColor('#956600') }
        ForEach(this.automation.schedules.concat(this.automation.archived ?? []), (plan: ScheduleDto) => {
          Column({ space: 8 }) {
            Text(plan.name).fontSize(20).fontWeight(FontWeight.Bold)
            Text(plan.archived ? '历史计划，当前定义已移除' : plan.enabled ? '已启用' : '已停用')
            Text(plan.schedule).copyOption(CopyOptions.InApp)
            if (plan.next_trigger) { Text(`下次运行：${plan.next_trigger}`).fontSize(13) }
            Text(plan.content).fontSize(14)
            if (plan.health.alerting) { Text(`连续失败 ${plan.health.consecutive_failures} 次`).fontColor('#B64040') }
            Row({ space: 8 }) {
              if (!plan.archived) {
                Button('编辑').enabled(!this.busy && !this.hasPending && !this.automation.stale).onClick(() => { this.editPlan(plan); })
                Button(plan.enabled ? '停用' : '启用').enabled(this.connected && !this.busy && !this.hasPending && !this.automation.stale).onClick(() => { this.togglePlan(plan); })
                Button('删除').enabled(this.connected && !this.busy && !this.hasPending && !this.automation.stale).onClick(() => {
                  this.manageSchedule({ kind: 'delete', schedule_id: plan.id, expected_revision: plan.revision });
                })
              }
              Button('记录').enabled(this.connected && !this.busy && !this.hasPending).onClick(() => { this.manageSchedule({ kind: 'history', schedule_id: plan.id }); })
            }
          }.alignItems(HorizontalAlign.Start).padding(16).backgroundColor('#FFFFFF').borderRadius(12).width('100%')
        }, (plan: ScheduleDto) => JSON.stringify(plan))
        if (this.automation.archive_truncated) { Text('部分计划超出当前归档展示上限。') }
        if (this.automation.history) {
          Text('最近运行记录（最多 50 条）').fontSize(20).fontWeight(FontWeight.Bold)
          ForEach(this.automation.history.records, (run: RunDto) => {
            Column({ space: 6 }) {
              Text(run.fired_at).fontSize(13)
              Text(run.outcome === 'notified' ? '通道已接受，设备接收未确认' : run.outcome)
              Text(run.result_summary).fontSize(14)
              if (run.error) { Text(run.error).fontColor('#B64040') }
            }.alignItems(HorizontalAlign.Start).padding(12).backgroundColor('#FFFFFF').width('100%')
          })
          if (this.automation.history.records.length === 0) { Text('还没有运行记录') }
        }
        Text(this.editPlanId ? '编辑计划' : '创建计划').fontSize(22).fontWeight(FontWeight.Bold)
        TextInput({ placeholder: '计划名称', text: this.planName }).onChange((value: string) => { this.planName = value; })
        TextArea({ placeholder: '每次运行的任务内容', text: this.planContent }).height(100).onChange((value: string) => { this.planContent = value; })
        TextInput({ placeholder: 'cron:0 8 * * * 或 once:时间', text: this.planExpression }).onChange((value: string) => { this.planExpression = value; })
        Row({ space: 12 }) {
          Text('启用'); Toggle({ type: ToggleType.Switch, isOn: this.planEnabled }).onChange((value: boolean) => { this.planEnabled = value; })
          Button(this.planNotify === 'reply' ? '结果自动回复' : '仅显式通知').onClick(() => { this.planNotify = this.planNotify === 'reply' ? 'explicit' : 'reply'; })
        }
        Row({ space: 12 }) {
          Button('保存计划').enabled(this.connected && !this.busy && !this.hasPending && this.connection.supports('schedules.manage')).onClick(() => { this.savePlan(); })
          Button('新建表单').onClick(() => { this.resetPlan(); })
        }
      }.width('100%').padding(20).constraintSize({ maxWidth: 850 })
    }.width('100%').layoutWeight(1)
  }

  @Builder private settingsView() {
    Scroll() {
      Column({ space: 16 }) {
        Text('连接你的任务工作台').fontSize(28).fontWeight(FontWeight.Bold).fontColor('#183153')
        Text('通过局域网或 EasyTier 使用 HTTPS。请先核对服务端实例 ID；自签证书需要可信 CA。').fontColor('#526070')
        TextInput({ placeholder: 'https://主机名:9443', text: this.address }).onChange((value: string) => { this.address = value; })
        TextInput({ placeholder: '实例 ID', text: this.instance }).onChange((value: string) => { this.instance = value; })
        TextInput({ placeholder: '一次性配对码（首次连接）', text: this.pairingCode }).type(InputType.Password).onChange((value: string) => { this.pairingCode = value; })
        TextArea({ placeholder: '私有 CA 的 PEM 证书（使用系统可信证书时留空）', text: this.caPem }).height(130).onChange((value: string) => { this.caPem = value; })
        Button('配对并连接').enabled(!this.busy && this.pairingCode.length > 0).onClick(() => { this.connect(true); })
        Button('使用已保存凭证连接').enabled(!this.busy).onClick(() => { this.connect(false); })
        Text('凭证保存在系统安全存储中。重要通知需服务端配置；文件通过授权 ID 访问。').fontSize(13).fontColor('#526070')
      }.padding(24).width('100%').constraintSize({ maxWidth: 620 })
    }.width('100%').layoutWeight(1)
  }

  // 普通 DTO 的同 ID 对象会整体替换；行键包含版本/可变内容，避免 ForEach 复用旧闭包。
  @Builder private sessionList() {
    Column({ space: 12 }) {
      Row() {
        Text('会话').fontSize(22).fontWeight(FontWeight.Bold).layoutWeight(1)
        Button('新建').enabled(this.connected && !this.busy && !this.hasPending && this.connection.supports('sessions.create')).onClick(() => {
          const command: CommandDto = { operation: 'create_session', command_id: util.generateRandomUUID(), title: '新的工作' };
          this.submit(command);
        })
      }
      TextInput({placeholder: '搜索本地会话历史', text: this.sessionSearch}).onChange((value: string) => { this.sessionSearch = value; })
      List({ space: 8 }) {
        ForEach(this.sessions.filter((session: SessionDto) => !this.sessionSearch || session.title.includes(this.sessionSearch) || session.messages.some((message: MessageDto) => message.content.includes(this.sessionSearch))), (session: SessionDto) => {
          ListItem() {
            Column({ space: 6 }) {
              Text(session.title).fontWeight(FontWeight.Medium)
              Text(`${session.messages.length} 条消息 · ${Object.keys(session.tasks).length} 个任务`).fontSize(12).fontColor('#526070')
            }.width('100%').alignItems(HorizontalAlign.Start).padding(14)
              .backgroundColor(this.selected === session.id ? '#DDF3ED' : '#FFFFFF').borderRadius(12)
          }.onClick(() => { this.select(session.id); })
        }, (session: SessionDto) => `${session.id}:${session.version}`)
      }.layoutWeight(1)
      if (this.sessions.length === 0) { Text('创建会话，开始一项工作').fontColor('#526070') }
    }.padding(16).height('100%').width(this.windowWidth < 700 ? '100%' : 280)
  }

  @Builder private detailView() {
    Column({ space: 12 }) {
      Text('任务与审批').fontSize(20).fontWeight(FontWeight.Bold)
      ForEach(this.tasks(), (task: TaskDto) => {
        Column({ space: 6 }) {
          Text(task.name).maxLines(3)
          if (task.parent_id) { Text(`子任务 · 父任务 ${task.parent_id}`).fontSize(12) }
          Text(task.status).fontSize(13).fontColor('#526070')
          if (task.result) { Text(task.result).fontSize(13) }
          Row({ space: 8 }) {
            if (task.can_stop && this.connection.supports('tasks.stop')) {
              Button('停止本轮').enabled(this.connected && !this.busy && !this.hasPending).onClick(() => { this.control(task, 'stop_task'); })
            }
            if (task.can_finish && this.connection.supports('tasks.finish')) {
              Button('结束任务').enabled(this.connected && !this.busy && !this.hasPending).onClick(() => { this.control(task, 'finish_task'); })
            }
          }
        }.alignItems(HorizontalAlign.Start).width('100%').padding(12).backgroundColor('#FFFFFF').borderRadius(12)
      }, (task: TaskDto) => JSON.stringify(task))
      ForEach(Object.values(this.current()?.progress ?? {}), (progress: ProgressDto) => {
        Column({space:6}) {
          if (progress.batch_id) { Text(`子任务批次：${progress.completed ?? 0} / ${progress.total ?? 0}`).fontSize(13) }
          if (progress.pending !== undefined) { Text(`待消费输入：${progress.pending}`).fontSize(13) }
          ForEach(progress.items ?? [], (todo: TodoDto) => { Text(`${todo.status === 'completed' ? '✓' : todo.status === 'in_progress' ? '▶' : '○'} ${todo.content}`).fontSize(14) })
        }.width('100%').alignItems(HorizontalAlign.Start)
      })
      ForEach(this.approvals(), (request: ApprovalDto) => {
        Column({ space: 8 }) {
          Text(`${request.agent_name} 请求 ${request.tool_name}`).fontWeight(FontWeight.Bold)
          Text(request.input_json ?? '').fontSize(12).copyOption(CopyOptions.InApp)
          Text(request.context ?? '').fontSize(12)
          TextArea({placeholder: '审批反馈（可选）', text: this.feedback(request)}).enabled(!request.claimed).onChange((value: string) => { this.editFeedback(request, value); })
          if (request.claimed) { Text('已有设备提交，等待运行时校验').fontColor('#526070') }
          ForEach(request.options, (option: ApprovalOptionDto) => {
            Button(option.label).enabled(!request.claimed && this.connected && !this.busy && !this.hasPending && this.connection.supports('approvals.respond'))
              .onClick(() => { this.approve(request, option.id); })
          })
        }.padding(12).backgroundColor('#FFF4DA').borderRadius(12).width('100%')
      }, (request: ApprovalDto) => `${request.request_id}:${request.version}:${request.claimed}`)
      ForEach(this.questions(), (question: QuestionDto) => {
        Column({ space: 8 }) {
          Text('等待你的回答').fontWeight(FontWeight.Bold)
          Text(question.question).copyOption(CopyOptions.InApp)
          if (question.claimed) { Text('已有设备提交回答，等待运行时处理') }
          else {
            TextArea({text: this.questionDraft(question), placeholder: '回答这个问题'})
              .onChange((value: string) => { this.editAnswer(question, value); })
            Button('提交回答').enabled(this.connected && !this.busy && !this.hasPending && this.connection.supports('questions.answer'))
              .onClick(() => { this.answerQuestion(question); })
          }
        }.width('100%').padding(12).backgroundColor('#EAF1FC').borderRadius(12)
      }, (question: QuestionDto) => `${question.task_id}/${question.question_id}:${question.claimed}`)
    }.padding(16).width('100%').alignItems(HorizontalAlign.Start)
  }

  @Builder private conversation() {
    Column({ space: 12 }) {
      Row() {
        if (this.windowWidth < 700) { Button('返回').onClick(() => { this.select(''); }) }
        Text(this.current()?.title ?? '选择会话').fontSize(22).fontWeight(FontWeight.Bold).layoutWeight(1)
      }
      Row({ space: 6 }) {
        Button('对话').onClick(() => { this.workspaceTab = 'chat'; })
        Button('进度').onClick(() => { this.workspaceTab = 'progress'; })
        Button('执行').onClick(() => { this.workspaceTab = 'execution'; })
        Button('产物').onClick(() => { this.workspaceTab = 'artifacts'; })
      }
      if (this.approvals().length + this.questions().length > 0) {
        Text(`待处理：${this.approvals().length} 项审批，${this.questions().length} 个问题`).fontColor('#956600')
          .onClick(() => { this.workspaceTab = 'progress'; })
      }
      if (this.workspaceTab === 'chat') {
      Scroll(this.conversationScroller) {
        Column({ space: 12 }) {
          if (this.current()?.history_truncated) { Text('较早的消息已超出当前保留范围').fontSize(12) }
          ForEach(this.current()?.messages ?? [], (message: MessageDto) => {
            Column({ space: 6 }) {
              Text(message.role === 'agent' ? 'Harness' : message.role === 'user' ? '你' : '系统').fontSize(12).fontColor('#526070')
              Text(message.content).fontSize(16).copyOption(CopyOptions.InApp)
            }.width('100%').alignItems(HorizontalAlign.Start).padding(16)
              .backgroundColor(message.role === 'user' ? '#DDF3ED' : '#FFFFFF').borderRadius(14)
          })

        }.width('100%')
      }.layoutWeight(1).width('100%')
      } else if (this.workspaceTab === 'progress') { Scroll() { this.detailView() }.layoutWeight(1).width('100%') }
      else if (this.workspaceTab === 'execution') { this.executionView() }
      else { this.artifactsView() }
      if (this.attachments.length > 0) {
        Row({ space: 8 }) {
          Text(`已附加 ${this.attachments.length} 个文件`).fontSize(12).layoutWeight(1)
          Button('清除附件选择').enabled(!this.busy && !this.hasPending).onClick(() => { this.attachments = []; this.save(`attachments:${this.selected}`, '[]'); })
        }
      }
      Row({ space: 8 }) {
        TextArea({ placeholder: '输入目标；运行中的输入进入同一任务', text: this.draft }).height(80).layoutWeight(1)
          .onChange((value: string) => { this.draft = value; this.save(`draft:${this.selected}`, value); })
        Button('发送').enabled(this.connected && !this.busy && !this.hasPending && this.selected.length > 0 && this.connection.supports('sessions.text'))
          .onClick(() => { this.send(); })
      }
    }.padding(16).height('100%').layoutWeight(1)
  }

  build() {
    Column() {
      Row({ space: 12 }) {
        Text('Harness').fontSize(24).fontWeight(FontWeight.Bold).fontColor('#183153').layoutWeight(1)
        if (!this.settings) {
          Button('重连').enabled(!this.busy).onClick(() => { this.connect(false); })
          Button('设置').onClick(() => { this.cancelReconnect(); this.connection.close(); this.connected = false; this.settings = true; })
        }
      }.padding(16).width('100%')
      if (!this.settings) {
        Row({ space: 12 }) {
          Button('会话').onClick(() => { this.view = 'sessions'; })
          Button('自动化').enabled(this.connection.supports('schedules.manage')).onClick(() => { this.view = 'automation'; this.manageSchedule({ kind: 'list' }); })
          Button('收件箱').onClick(() => { this.view = 'inbox'; })
          Button('管理').enabled(this.connection.supports('devices.list')).onClick(() => { this.view = 'management'; this.loadManagement(); })
        }.padding({ left: 16, right: 16, bottom: 10 }).width('100%')
      }
      Text(this.status).fontSize(13).fontColor('#526070').width('100%').padding({ left: 16, right: 16, bottom: 12 })
      if (this.hasPending) {
        Column({ space: 6 }) {
          Text('存在待发送或结果未知的命令。清除本地记录不会撤销服务器操作。').fontSize(12)
          Row({ space: 6 }) {
            Button('查状态').enabled(this.connected).onClick(() => { this.queryPending(); })
            Button('重试').enabled(this.connected && !this.busy).onClick(() => { if (this.pending) { this.submit(this.pending); } })
            Button('清除本地重试').enabled(!this.busy).onClick(() => { this.pending = undefined; this.hasPending = false; this.save('pending', ''); })
          }
        }.padding(8)
      }
      if (this.settings) { this.settingsView() }
      else if (this.view === 'automation') { this.automationView() }
      else if (this.view === 'inbox') { this.inboxView() }
      else if (this.view === 'management') { this.managementView() }
      else {
        Row() {
          if (this.windowWidth >= 700 || this.selected.length === 0) { this.sessionList() }
          if (this.windowWidth >= 700 || this.selected.length > 0) { this.conversation() }
          if (this.windowWidth >= 1100) { Scroll() { this.detailView() }.width(320).height('100%') }
        }.layoutWeight(1).width('100%').alignItems(VerticalAlign.Top)
      }
    }.height('100%').width('100%').backgroundColor('#F5F7FA')
      .onAreaChange((_old, area) => {
        const width = Number(area.width);
        if (width === this.windowWidth) { return; }
        if (!this.settings && this.view === 'sessions' && this.workspaceTab === 'chat' && this.selected) {
          try {
            const y = this.conversationScroller.currentOffset().yOffset;
            const session = this.selected;
            this.windowWidth = width;
            setTimeout(() => {
              if (this.selected === session && this.workspaceTab === 'chat' && this.view === 'sessions') {
                try { this.conversationScroller.scrollTo({ xOffset: 0, yOffset: y }); } catch (_) { }
              }
            }, 0);
          } catch (_) { this.windowWidth = width; }
        } else { this.windowWidth = width; }
      })
  }
}
