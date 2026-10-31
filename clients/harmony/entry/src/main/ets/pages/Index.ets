import { common } from '@kit.AbilityKit';
import { preferences } from '@kit.ArkData';
import { fileIo, fileUri, picker } from '@kit.CoreFileKit';
import { util } from '@kit.ArkTS';
import { deviceInfo } from '@kit.BasicServicesKit';
import { scanBarcode, scanCore } from '@kit.ScanKit';
import { Connection, CredentialStore, PairingLink, parseLink, trustCa } from '../services/Connection';
import { ApprovalDto, ApprovalOptionDto, AutomationDto, ClientFileDto, CommandDto, DeviceDto, DiagnosticsDto, MessageDto, NotificationDto, ObservationDto, PendingUploadDto, ProgressDto, QuestionDto, RunDto, ScheduleActionDto, ScheduleDto, ScheduleInputDto, SessionDto, TaskDto, TodoDto } from '../services/Protocol';
import { Theme } from '../common/Theme';
import { conversationRows, ConversationRow, ProcessEntry } from '../services/Conversation';
import { MarkdownView } from '../components/MarkdownView';
import { ArtifactRef, ArtifactViewer, AttachmentThumb, artifactIcon, fileArtifact, linkArtifacts } from '../components/ArtifactViewer';

@Extend(Button) function ghost() {
  .type(ButtonType.Normal).height(32).fontSize(13).fontColor(Theme.text).backgroundColor(Color.Transparent)
  .border({ width: 1, color: Theme.line, radius: 16 }).padding({ left: 14, right: 14 })
}
@Extend(Button) function primary() {
  .type(ButtonType.Normal).height(40).fontSize(15).fontColor(Color.White).backgroundColor(Theme.accent)
  .borderRadius(20).padding({ left: 22, right: 22 })
}
@Extend(Button) function icon() {
  .type(ButtonType.Circle).width(36).height(36).backgroundColor(Color.Transparent)
}
@Extend(Column) function card() {
  .width('100%').alignItems(HorizontalAlign.Start).padding(16).backgroundColor(Theme.surface)
  .border({ width: 1, color: Theme.line, radius: 14 })
}
@Extend(Text) function caption() {
  .fontSize(12).fontColor(Theme.secondary)
}
@Extend(Text) function heading(size: number) {
  .fontSize(size).fontColor(Theme.text).fontFamily(Theme.serif).fontWeight(FontWeight.Medium)
}

@Entry
@Component
struct Index {
  @State processExpanded: Record<string, boolean> = {};
  @State instance: string = '';
  @State fingerprint: string = '';
  @State addresses: string[] = [];
  @State preferred: string = '';
  @State linkText: string = '';
  @State status: string = '';
  @State sessions: SessionDto[] = [];
  @State selected: string = '';
  @State draft: string = '';
  @State windowWidth: number = 390;
  @State paired: boolean = false;
  @State connected: boolean = false;
  @State busy: boolean = false;
  @State hasPending: boolean = false;
  @State questionDrafts: Record<string, string> = {};
  @State view: string = 'chat';
  @State sidebarOpen: boolean = false;
  @State panelOpen: boolean = false;
  @State panelTab: string = 'artifacts';
  @State searching: boolean = false;
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
  @State devices: DeviceDto[] = [];
  @State diagnostics: DiagnosticsDto | undefined = undefined;
  @State viewing: ArtifactRef | undefined = undefined;
  @State sessionSearch: string = '';
  @State approvalFeedback: Record<string, string> = {};
  private conversationScroller: Scroller = new Scroller();
  private editPlanRevision: string = '';
  @StorageLink('harnessForeground') @Watch('lifecycle') foreground: boolean = true;
  // 沉浸式避让高度（px），由 EntryAbility 按系统状态栏与导航条更新。
  @StorageProp('safeTop') safeTop: number = 0;
  @StorageProp('safeBottom') safeBottom: number = 0;
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
      this.loadPairing();
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
    if (this.paired) { this.connect(''); }
  }
  aboutToDisappear(): void { this.cancelReconnect(); this.connection.close(); }

  // 配对信息只在配对成功后整体写入；重新配对失败时可回到已保存的连接。
  private loadPairing(): void {
    this.instance = this.storage?.getSync('pair.instance', '') as string ?? '';
    this.fingerprint = this.storage?.getSync('pair.fingerprint', '') as string ?? '';
    this.preferred = this.storage?.getSync('pair.preferred', '') as string ?? '';
    try { this.addresses = JSON.parse(this.storage?.getSync('pair.addresses', '[]') as string ?? '[]') as string[]; }
    catch (_) { this.addresses = []; }
    this.paired = this.instance.length > 0 && this.addresses.length > 0;
  }
  private savedInstance(): string { try { return this.storage?.getSync('pair.instance', '') as string ?? ''; } catch (_) { return ''; } }
  private top(): number { return this.getUIContext().px2vp(this.safeTop); }
  private bottom(): number { return this.getUIContext().px2vp(this.safeBottom); }
  // 侧栏与右侧面板的显隐统一走同一曲线，内嵌布局的宽度变化随之过渡。
  private setSidebar(open: boolean): void {
    this.getUIContext().animateTo({ duration: 300, curve: Curve.Friction }, () => { this.sidebarOpen = open; });
  }
  private setPanel(open: boolean): void {
    this.getUIContext().animateTo({ duration: 300, curve: Curve.Friction }, () => { this.panelOpen = open; });
  }
  private wide(): boolean { return this.windowWidth >= Theme.wide; }
  private cancelReconnect(): void { if (this.reconnectTimer >= 0) { clearTimeout(this.reconnectTimer); this.reconnectTimer = -1; } }
  private scheduleReconnect(): void {
    if (!this.paired || !this.foreground || this.reconnectTimer >= 0) { return; }
    this.reconnectTimer = setTimeout(() => { this.reconnectTimer = -1; this.connect(''); }, this.reconnectDelay);
    this.reconnectDelay = Math.min(this.reconnectDelay * 2, 30000);
  }
  private lifecycle(): void {
    if (!this.foreground) { this.cancelReconnect(); this.connection.close(); this.connected = false; }
    else if (this.paired) { this.connect(''); }
  }
  private save(key: string, value: string): boolean {
    try {
      if (!this.storage) { throw new Error('storage unavailable'); }
      this.storage.putSync(key, value); this.storage.flushSync(); return true;
    } catch (_) { this.status = '无法保存本地状态，尚未发送新命令'; return false; }
  }
  private select(id: string): void {
    this.viewing = undefined;
    this.selected = id; this.save('selected', id); this.view = 'chat';
    if (!this.wide() && this.sidebarOpen) { this.setSidebar(false); }
    try { this.draft = this.storage?.getSync(`draft:${id}`, '') as string ?? ''; }
    catch (_) { this.status = '无法读取草稿'; this.draft = ''; }
    try { this.attachments = JSON.parse(this.storage?.getSync(`attachments:${id}`, '[]') as string ?? '[]') as string[]; }
    catch (_) { this.attachments = []; }
  }
  private current(): SessionDto | undefined { return this.sessions.find((session: SessionDto) => session.id === this.selected); }
  private tasks(): TaskDto[] { const session = this.current(); return session ? Object.values(session.tasks) : []; }
  private approvals(): ApprovalDto[] { const session = this.current(); return session ? Object.values(session.approvals) : []; }
  private questions(): QuestionDto[] { const session = this.current(); return session ? Object.values(session.questions ?? {}) : []; }
  private todos(): TodoDto[] {
    const session = this.current();
    if (!session) { return []; }
    let items: TodoDto[] = [];
    for (const progress of Object.values(session.progress ?? {})) { items = items.concat(progress.items ?? []); }
    return items;
  }
  private sessionFiles(): ClientFileDto[] { return this.files.filter((file: ClientFileDto) => file.session_id === this.selected); }
  private statusLabel(status: string): string {
    if (status === 'running') { return '执行中'; }
    if (status === 'pending') { return '排队中'; }
    if (status === 'waiting') { return '等待回复'; }
    if (status === 'done') { return '已完成'; }
    if (status === 'failed') { return '失败'; }
    if (status === 'recovery_required') { return '需恢复'; }
    return status;
  }
  private sessionState(session: SessionDto): string {
    const roots = Object.values(session.tasks).filter((task: TaskDto) => !task.parent_id);
    if (Object.keys(session.approvals).length + (session.questions ? Object.keys(session.questions).length : 0) > 0) { return '待处理'; }
    if (roots.some((task: TaskDto) => task.status === 'running' || task.status === 'pending')) { return '执行中'; }
    if (roots.some((task: TaskDto) => task.status === 'waiting')) { return '等待回复'; }
    if (roots.length === 0) { return '空闲'; }
    return this.statusLabel(roots[roots.length - 1].status);
  }
  private headerTitle(): string {
    if (this.view !== 'chat') { return ''; }
    const session = this.current();
    return session ? `任务 · ${this.sessionState(session)}` : '新任务';
  }
  private unread(): number { return this.notifications.filter((n: NotificationDto) => !n.read_at).length; }
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

  private async scan(): Promise<void> {
    try {
      const context = this.getUIContext().getHostContext() as common.UIAbilityContext;
      const options: scanBarcode.ScanOptions = { scanTypes: [scanCore.ScanType.QR_CODE], enableMultiMode: false, enableAlbum: true };
      const result = await scanBarcode.startScanForResult(context, options);
      this.linkText = result.originalValue;
      await this.pairWith(result.originalValue);
    } catch (_) { this.status = '扫码未完成，可改为粘贴配对链接'; }
  }
  private async pairWith(text: string): Promise<void> {
    let link: PairingLink;
    try { link = parseLink(text); } catch (error) { this.status = (error as Error).message; return; }
    this.instance = link.instance; this.fingerprint = link.fingerprint; this.addresses = link.addresses; this.preferred = '';
    await this.connect(link.code);
    if (!this.connected) { return; }
    this.save('pair.instance', link.instance); this.save('pair.fingerprint', link.fingerprint);
    this.save('pair.addresses', JSON.stringify(link.addresses));
    this.paired = true; this.linkText = ''; this.sidebarOpen = this.wide();
  }
  private unpair(): void {
    this.cancelReconnect(); this.connection.close(); this.connected = false; this.paired = false; this.status = '';
  }

  // 依次尝试配对链接中的地址：先按指纹信任实例 CA，再核对实例身份，之后才发送设备凭证。
  private async connect(code: string): Promise<void> {
    if (this.busy) { return; }
    this.busy = true;
    this.cancelReconnect();
    this.connected = false;
    this.status = code ? '正在配对…' : '正在连接…';
    try {
      if (this.hasPending && this.storage?.getSync('pending-instance', '') !== this.instance) {
        throw new Error('另一个实例仍有待发送命令，请先处理或清除本地重试');
      }
      const context = this.getUIContext().getHostContext() as common.UIAbilityContext;
      const ordered = this.addresses.includes(this.preferred) ?
        [this.preferred].concat(this.addresses.filter((host: string) => host !== this.preferred)) : this.addresses;
      let chosen = '';
      const failures: string[] = [];
      this.connection.connectTimeout = 4000;
      for (const host of ordered) {
        const address = `https://${host}`;
        try {
          const caPath = this.fingerprint ? await trustCa(address, this.fingerprint, context.filesDir) : '';
          this.connection.configure(address, this.instance, caPath);
          await this.connection.checkIdentity();
          chosen = host;
          break;
        } catch (error) { failures.push(`${host} ${(error as Error).message}`); }
      }
      this.connection.connectTimeout = 10000;
      if (!chosen) {
        throw new Error(failures.length > 0 ? `无法连接实例，已尝试 ${failures.length} 个地址：${failures.join('；')}` : '没有可用的实例地址');
      }
      if (code) {
        await this.connection.pair(code, (deviceInfo.marketName || 'HarmonyOS 设备').substring(0, 64));
      } else {
        this.connection.credential = await new CredentialStore().load(this.instance);
        if (!this.connection.credential) { throw new Error('本机没有该实例的凭证，请重新配对'); }
        await this.connection.verify();
      }
      this.preferred = chosen; this.save('pair.preferred', chosen);
      const snapshot = await this.connection.snapshot();
      if (!this.foreground) { this.connection.close(); return; }
      this.applySnapshot(snapshot.sessions, snapshot.automation, snapshot.notifications, snapshot.notifications_truncated, snapshot.notification_overflow, snapshot.files);
      this.connected = true;
      this.status = '';
      this.reconnectDelay = 1000;
      await this.connection.subscribe(snapshot.cursor, () => { this.refresh(); }, (text: string) => {
        this.status = text; this.connected = false;
        if (text !== 'unauthorized') { this.scheduleReconnect(); }
      });
    } catch (error) {
      this.connection.close(); this.connected = false;
      this.status = (error as Error).message || '连接失败';
      if (!code && !this.status.includes('HTTP 401')) { this.scheduleReconnect(); }
    } finally { this.busy = false; this.connection.connectTimeout = 10000; }
  }
  private applySnapshot(sessions: SessionDto[], automation: AutomationDto | undefined, notifications: NotificationDto[] | undefined,
    truncated: boolean | undefined, overflow: number | undefined, files: ClientFileDto[] | undefined): void {
    // 新回复到达时收起过程，即使用户此前曾手动展开运行中的过程。
    const previous = this.sessions.find((session: SessionDto) => session.id === this.selected);
    const updated = sessions.find((session: SessionDto) => session.id === this.selected);
    if (previous && updated) {
      const oldRows = conversationRows(previous);
      const next: Record<string, boolean> = {};
      for (const key of Object.keys(this.processExpanded)) { next[key] = this.processExpanded[key]; }
      for (const row of conversationRows(updated)) {
        if (row.complete && oldRows.some((old: ConversationRow) => old.key === row.key && !old.complete)) { next[row.key] = false; }
      }
      this.processExpanded = next;
    }
    this.sessions = sessions;
    this.automation = automation ?? { available: false, schedules: [], timezone: '' };
    this.notifications = notifications ?? []; this.notificationsTruncated = truncated ?? false; this.notificationOverflow = overflow ?? 0;
    this.files = files ?? [];
    if (this.selected && !sessions.some((session: SessionDto) => session.id === this.selected)) { this.selected = ''; this.save('selected', ''); }
  }

  private async refresh(): Promise<void> {
    if (this.refreshing) { this.refreshAgain = true; return; }
    this.refreshing = true;
    try {
      do {
        this.refreshAgain = false;
        const snapshot = await this.connection.snapshot();
        this.applySnapshot(snapshot.sessions, snapshot.automation, snapshot.notifications, snapshot.notifications_truncated, snapshot.notification_overflow, snapshot.files);
        if (this.lastCommandId.length > 0) {
          const receipt = await this.connection.status(this.lastCommandId);
          if (receipt.status === 'rejected') { this.status = `命令被拒绝：${receipt.result_code ?? ''}`; }
        }
      } while (this.refreshAgain);
    } catch (error) { this.status = (error as Error).message; }
    finally { this.refreshing = false; }
  }

  private async submit(command: CommandDto): Promise<void> {
    if (this.busy || !this.connected) { return; }
    this.busy = true;
    this.pending = command; this.hasPending = true;
    if (!this.save('pending-instance', this.instance) || !this.save('pending', JSON.stringify(command))) { this.busy = false; return; }
    try {
      const receipt = await this.connection.command(command);
      this.lastCommandId = command.command_id;
      this.status = receipt.status === 'rejected' ? `命令被拒绝：${receipt.result_code ?? ''}` : '';
      if (command.operation === 'create_session') { this.select(receipt.session_id); }
      if (command.operation === 'send_text' && receipt.status === 'queued') {
        this.save(`draft:${command.session_id ?? ''}`, '');
        if (command.session_id === this.selected) { this.draft = ''; }
        this.save(`attachments:${command.session_id ?? ''}`, '[]');
        if (command.session_id === this.selected) { this.attachments = []; }
      }
      this.pending = undefined; this.hasPending = false; this.save('pending', '');
      await this.refresh();
    } catch (error) { this.status = `${(error as Error).message}。已保留原命令，可查询状态或重试`; }
    finally { this.busy = false; }
  }

  private async queryPending(): Promise<void> {
    if (!this.pending) { return; }
    try { const receipt = await this.connection.status(this.pending.command_id); this.status = `服务器命令状态：${receipt.status} ${receipt.result_code ?? ''}`; }
    catch (error) { this.status = (error as Error).message; }
  }
  // 新任务在首次发送时才创建会话，标题取自首行内容。
  private async send(): Promise<void> {
    if ((!this.draft.trim() && this.attachments.length === 0) || this.hasPending || this.busy) { return; }
    const text = this.draft;
    if (!this.current()) {
      const title = text.trim().split('\n')[0].substring(0, 24) || '新的任务';
      await this.submit({ operation: 'create_session', command_id: util.generateRandomUUID(), title: title });
      if (!this.selected) { return; }
      this.draft = text; this.save('draft:', '');
    }
    const command: CommandDto = { operation: 'send_text', command_id: util.generateRandomUUID(), session_id: this.selected, content: text, attachment_ids: this.attachments };
    await this.submit(command);
    setTimeout(() => { try { this.conversationScroller.scrollEdge(Edge.Bottom); } catch (_) { } }, 50);
  }
  private newTask(): void { this.select(''); if (this.panelOpen) { this.setPanel(false); } }
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
    if (this.busy) { return; }
    if (!this.selected) { this.status = '先发送第一条消息创建任务，再添加附件'; return; }
    this.busy = true;
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
        this.pendingUpload = { instance_id: this.instance, id: id, session_id: this.selected, name: new fileUri.FileUri(selected[0]).name || 'attachment.bin', local_path: local };
        if (!this.save('pending-upload', JSON.stringify(this.pendingUpload))) { return; }
      }
      const pending = this.pendingUpload;
      if (!pending) { return; }
      if (pending.instance_id !== this.instance) { throw new Error('待上传文件属于其他实例，请切回原实例'); }
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
      this.status = ''; await this.refresh();
    } catch (error) { this.status = `${(error as Error).message}。已保留上传缓存，可重试`; }
    finally { this.busy = false; if (!this.connected) { this.scheduleReconnect(); } }
  }
  private openArtifact(ref: ArtifactRef): void {
    this.getUIContext().animateTo({ duration: 300, curve: Curve.Friction }, () => {
      this.viewing = ref; this.panelTab = 'artifacts'; this.panelOpen = true;
      // 查看器加宽后若对话区过窄，收起内嵌侧栏让出空间。
      if (this.wide() && this.sidebarOpen && this.windowWidth - 272 - this.panelWidth() < 560) { this.sidebarOpen = false; }
    });
  }
  private closeArtifact(): void {
    this.getUIContext().animateTo({ duration: 300, curve: Curve.Friction }, () => { this.viewing = undefined; });
  }
  // 产物已以卡片展示，隐藏通道消息里的原机路径标记。
  private visibleText(content: string): string {
    return content.replace(/\[(IMAGE|VIDEO|AUDIO|DOCUMENT|FILE)[^\]]*\]/gi, '').trim();
  }
  private fileById(id: string): ClientFileDto | undefined { return this.files.find((file: ClientFileDto) => file.id === id); }
  private messageFiles(message: MessageDto): ClientFileDto[] {
    const files: ClientFileDto[] = [];
    for (const id of message.attachment_ids ?? []) { const file = this.fileById(id); if (file) { files.push(file); } }
    return files;
  }
  private sessionLinks(): ArtifactRef[] {
    const texts = (this.current()?.messages ?? []).filter((m: MessageDto) => m.role !== 'user').map((m: MessageDto) => m.content);
    return linkArtifacts(texts, this.preferred);
  }
  private panelWidth(): number { return this.viewing ? Math.max(440, Math.floor(this.windowWidth * 0.45)) : 380; }
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
  private approve(request: ApprovalDto, option: string): void {
    const command: CommandDto = { operation: 'respond_approval', command_id: util.generateRandomUUID(), session_id: this.selected,
      request_id: request.request_id, expected_version: request.version, option_id: option, feedback: this.feedback(request) };
    this.submit(command);
  }
  private manageSchedule(action: ScheduleActionDto): void {
    if (!this.selected && this.sessions.length > 0) { this.selected = this.sessions[0].id; this.save('selected', this.selected); }
    if (!this.selected) { this.status = '请先创建一个任务，用于接收计划运行结果'; return; }
    const command: CommandDto = { operation: 'schedule', command_id: util.generateRandomUUID(), session_id: this.selected, action: action };
    this.submit(command);
  }
  private notificationState(value: string): string {
    if (value === 'accepted') { return 'MeoW 已接受，设备送达未确认'; }
    if (value === 'sending') { return '正在投递'; }
    if (value === 'unknown') { return '结果未知，可能已发出'; }
    if (value === 'quota_wait') { return '当天额度已用完'; }
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
  private openView(view: string): void {
    this.view = view;
    if (!this.wide() && this.sidebarOpen) { this.setSidebar(false); }
    if (view === 'automation' && this.connection.supports('schedules.manage')) { this.manageSchedule({ kind: 'list' }); }
    if (view === 'management') { this.loadManagement(); }
  }

  // ---------- 配对 ----------
  @Builder private onboarding() {
    Scroll() {
      Column({ space: 18 }) {
        Text('Harness').heading(40)
        Text('连接你的 Agent 工作台').fontSize(16).fontColor(Theme.secondary)
        Column({ space: 6 }) {
          Text('在运行 Harness 的电脑上执行').caption()
          Text('harness --client-pair <用户名>').fontSize(14).fontFamily('monospace').fontColor(Theme.text)
            .padding({ left: 12, right: 12, top: 8, bottom: 8 }).backgroundColor(Theme.surface).borderRadius(8)
            .border({ width: 1, color: Theme.line, radius: 8 }).copyOption(CopyOptions.LocalDevice)
          Text('然后扫描终端里的二维码；同一局域网或 EasyTier 内即可连接。').caption()
        }.alignItems(HorizontalAlign.Start).width('100%').margin({ top: 12 })
        Button() {
          Row({ space: 8 }) {
            SymbolGlyph($r('sys.symbol.qrcode')).fontSize(18).fontColor([Color.White])
            Text('扫码配对').fontSize(15).fontColor(Color.White)
          }
        }.primary().width('100%').enabled(!this.busy).onClick(() => { this.scan(); })
        Row({ space: 10 }) {
          Divider().color(Theme.line).layoutWeight(1)
          Text('或粘贴配对链接').caption()
          Divider().color(Theme.line).layoutWeight(1)
        }.width('100%')
        TextArea({ placeholder: 'harness://pair?…', text: this.linkText }).height(88).fontSize(13)
          .backgroundColor(Theme.surface).border({ width: 1, color: Theme.line, radius: 12 })
          .onChange((value: string) => { this.linkText = value; })
        Button('连接').ghost().height(40).border({ width: 1, color: Theme.line, radius: 20 }).width('100%').enabled(!this.busy && this.linkText.trim().length > 0)
          .onClick(() => { this.pairWith(this.linkText); })
        if (this.busy) { LoadingProgress().width(28).height(28).color(Theme.accent) }
        if (this.status) { Text(this.status).fontSize(13).fontColor(Theme.secondary).textAlign(TextAlign.Center) }
        if (this.savedInstance()) {
          Button('返回已保存的连接').ghost().enabled(!this.busy).onClick(() => { this.loadPairing(); if (this.paired) { this.connect(''); } })
        }
        Text('凭证保存在系统安全存储；实例证书按链接中的指纹校验后才被信任。').caption().textAlign(TextAlign.Center).margin({ top: 12 })
      }.width('100%').constraintSize({ maxWidth: 440 }).padding({ left: 28, right: 28, top: 80 + this.top(), bottom: 40 + this.bottom() })
    }.width('100%').height('100%').align(Alignment.Top).backgroundColor(Theme.page)
  }

  // ---------- 侧栏 ----------
  @Builder private navItem(symbol: Resource, label: string, view: string, badge: number) {
    Row({ space: 14 }) {
      SymbolGlyph(symbol).fontSize(19).fontColor([this.view === view ? Theme.text : Theme.secondary])
      Text(label).fontSize(15).fontColor(Theme.text).layoutWeight(1)
      if (badge > 0) { Text(`${badge}`).fontSize(11).fontColor(Color.White).backgroundColor(Theme.accent).borderRadius(8).padding({ left: 6, right: 6, top: 1, bottom: 1 }) }
    }.width('100%').height(44).padding({ left: 14, right: 14 }).borderRadius(10)
      .backgroundColor(this.view === view ? Theme.selected : Color.Transparent)
      .onClick(() => { this.openView(view); })
  }
  @Builder private sidebar() {
    Column() {
      Row() {
        Text('Harness').heading(26).layoutWeight(1)
        Button() { SymbolGlyph($r('sys.symbol.close_sidebar')).fontSize(20).fontColor([Theme.secondary]) }
          .icon().onClick(() => { this.setSidebar(false); })
      }.width('100%').margin({ top: 8, bottom: 20 })
      Button() {
        Row({ space: 8 }) {
          SymbolGlyph($r('sys.symbol.plus')).fontSize(16).fontColor([Theme.text])
          Text('新任务').fontSize(15).fontColor(Theme.text)
        }
      }.ghost().height(44).width('100%').border({ width: 1, color: Theme.line, radius: 10 }).onClick(() => { this.newTask(); })
      Column({ space: 4 }) {
        this.navItem($r('sys.symbol.house'), '工作台', 'chat', this.approvals().length + this.questions().length)
        this.navItem($r('sys.symbol.clock'), '自动化', 'automation', 0)
        this.navItem($r('sys.symbol.bell'), '通知', 'inbox', this.unread())
        this.navItem($r('sys.symbol.gearshape'), '设备与连接', 'management', 0)
      }.margin({ top: 18 })
      Divider().color(Theme.line).margin({ top: 18, bottom: 14 })
      Row() {
        Text('最近任务').caption().layoutWeight(1)
        Button() { SymbolGlyph($r('sys.symbol.magnifyingglass')).fontSize(16).fontColor([Theme.secondary]) }
          .icon().width(28).height(28).onClick(() => { this.searching = !this.searching; if (!this.searching) { this.sessionSearch = ''; } })
      }.padding({ left: 4 })
      if (this.searching) {
        TextInput({ placeholder: '搜索任务与消息', text: this.sessionSearch }).height(36).fontSize(13).margin({ top: 6, bottom: 6 })
          .backgroundColor(Theme.surface).onChange((value: string) => { this.sessionSearch = value; })
      }
      // 普通 DTO 的同 ID 对象会整体替换；行键包含版本，避免 ForEach 复用旧闭包。
      List({ space: 2 }) {
        ForEach(this.sessions.filter((session: SessionDto) => !this.sessionSearch || session.title.includes(this.sessionSearch) ||
          session.messages.some((message: MessageDto) => message.content.includes(this.sessionSearch)))
          .sort((a: SessionDto, b: SessionDto) => (b.updated_at ?? 0) - (a.updated_at ?? 0)), (session: SessionDto) => {
          ListItem() {
            Row({ space: 10 }) {
              Circle().width(7).height(7).fill(this.sessionState(session) === '执行中' || this.sessionState(session) === '待处理' ? Theme.accent : Theme.faint)
                .margin({ top: 7 })
              Column({ space: 3 }) {
                Text(session.title).fontSize(14).fontColor(Theme.text).maxLines(1).textOverflow({ overflow: TextOverflow.Ellipsis })
                Text(`${this.sessionState(session)} · ${session.messages.length} 条消息`).caption()
              }.alignItems(HorizontalAlign.Start).layoutWeight(1)
            }.alignItems(VerticalAlign.Top).width('100%').padding({ left: 10, right: 10, top: 10, bottom: 10 }).borderRadius(10)
              .backgroundColor(this.selected === session.id && this.view === 'chat' ? Theme.selected : Color.Transparent)
          }.onClick(() => { this.select(session.id); })
        }, (session: SessionDto) => `${session.id}:${session.version}`)
      }.layoutWeight(1).scrollBar(BarState.Off).margin({ top: 6 })
      Row({ space: 8 }) {
        Circle().width(8).height(8).fill(this.connected ? Theme.accent : Theme.faint)
        Text(this.connected ? (this.connection.capabilities?.instance_name ?? '已连接') : (this.busy ? '正在连接…' : '未连接'))
          .caption().layoutWeight(1).maxLines(1)
        if (!this.connected && !this.busy) {
          Text('重连').fontSize(12).fontColor(Theme.accent).onClick(() => { this.connect(''); })
        }
      }.width('100%').padding({ left: 6, top: 12, bottom: 4 })
    }.height('100%').width('100%').padding({ left: 16, right: 16, top: 12 + this.top(), bottom: 16 + this.bottom() }).backgroundColor(Theme.sidebar)
  }

  // ---------- 对话 ----------
  @Builder private header() {
    Row({ space: 4 }) {
      if (!this.sidebarOpen || !this.wide()) {
        Button() { SymbolGlyph($r('sys.symbol.open_sidebar')).fontSize(20).fontColor([Theme.secondary]) }
          .icon().onClick(() => { this.setSidebar(true); })
      }
      Text(this.headerTitle()).fontSize(14).fontColor(Theme.secondary).layoutWeight(1).maxLines(1).margin({ left: 4 })
      if (this.view === 'chat' && this.current()) {
        Button() {
          SymbolGlyph($r('sys.symbol.doc_text')).fontSize(19).fontColor([this.panelOpen ? Theme.accent : Theme.secondary])
        }.icon().onClick(() => { this.setPanel(!this.panelOpen); })
      }
    }.width('100%').height(52 + this.top()).padding({ left: 12, right: 12, top: this.top() })
  }
  // 消息附带的文件（上传附件或 Agent 产物）与回复中的链接，点击在右侧查看。
  @Builder private messageAttachments(message: MessageDto) {
    if (this.messageFiles(message).length > 0 || (message.role !== 'user' && linkArtifacts([message.content], this.preferred).length > 0)) {
      Flex({ wrap: FlexWrap.Wrap }) {
        ForEach(this.messageFiles(message), (file: ClientFileDto) => {
          if (fileArtifact(file).kind === 'image') {
            AttachmentThumb({ file: file, connection: this.connection })
              .margin({ right: 8, bottom: 8 }).onClick(() => { this.openArtifact(fileArtifact(file)); })
          } else { this.artifactChip(fileArtifact(file)) }
        }, (file: ClientFileDto) => file.id)
        if (message.role !== 'user') {
          ForEach(linkArtifacts([message.content], this.preferred), (ref: ArtifactRef) => { this.artifactChip(ref) }, (ref: ArtifactRef) => ref.key)
        }
      }.width('100%').margin({ top: 4 })
    }
  }
  @Builder private artifactChip(ref: ArtifactRef) {
    Row({ space: 10 }) {
      Stack() { SymbolGlyph(artifactIcon(ref.kind)).fontSize(16).fontColor([Theme.accent]) }
        .width(32).height(32).borderRadius(8).backgroundColor(Theme.accentSoft)
      Column({ space: 2 }) {
        Text(ref.title).fontSize(13).fontColor(Theme.text).maxLines(1).textOverflow({ overflow: TextOverflow.Ellipsis })
        Text(ref.detail).fontSize(11).fontColor(Theme.secondary)
      }.alignItems(HorizontalAlign.Start).constraintSize({ maxWidth: 220 })
    }.padding({ left: 8, right: 14, top: 8, bottom: 8 }).margin({ right: 8, bottom: 8 })
      .backgroundColor(this.viewing?.key === ref.key ? Theme.accentSoft : Theme.surface)
      .border({ width: 1, color: this.viewing?.key === ref.key ? Theme.accent : Theme.line, radius: 12 })
      .onClick(() => { this.openArtifact(ref); })
  }
  @Builder private message(message: MessageDto) {
    if (message.role === 'user' || message.role === 'agent') {
      Row({ space: 14 }) {
        Stack() {
          if (message.role === 'user') { SymbolGlyph($r('sys.symbol.person')).fontSize(16).fontColor([Theme.secondary]) }
          else { Text('H').fontSize(15).fontColor(Color.White).fontFamily(Theme.serif).fontWeight(FontWeight.Medium) }
        }.width(32).height(32).borderRadius(16).backgroundColor(message.role === 'user' ? Theme.selected : Theme.accent)
        Column({ space: 8 }) {
          if (message.role === 'user') {
            if (message.content !== '[附件]' || this.messageFiles(message).length === 0) {
              Text(message.content).fontSize(15).lineHeight(26).fontColor(Theme.text).copyOption(CopyOptions.LocalDevice).width('100%')
            }
          } else {
            if (message.phase === 'response') { Text('回复').caption() }
            MarkdownView({ source: message.content })
          }
          this.messageAttachments(message)
        }.layoutWeight(1).alignItems(HorizontalAlign.Start).margin({ top: 3 })
      }.width('100%').alignItems(VerticalAlign.Top)
    } else {
      Column({ space: 8 }) {
        if (this.visibleText(message.content)) {
          Text(this.visibleText(message.content)).caption().width('100%').copyOption(CopyOptions.LocalDevice)
        }
        this.messageAttachments(message)
      }.width('100%').padding({ left: 46 }).alignItems(HorizontalAlign.Start)
    }
  }
  private isProcessExpanded(row: ConversationRow): boolean {
    return this.processExpanded[row.key] ?? !row.complete;
  }
  @Builder private process(row: ConversationRow) {
    Column({ space: 10 }) {
      Row({ space: 8 }) {
        Text(this.isProcessExpanded(row) ? '⌄' : '›').fontSize(18).fontColor(Theme.secondary)
        Text(row.complete ? '执行过程' : '正在执行').fontSize(13).fontColor(Theme.secondary).fontWeight(FontWeight.Medium)
        Text(`${row.entries?.length ?? 0} 条记录`).caption()
      }.width('100%').onClick(() => {
        const next: Record<string, boolean> = {};
        for (const key of Object.keys(this.processExpanded)) { next[key] = this.processExpanded[key]; }
        next[row.key] = !this.isProcessExpanded(row);
        this.processExpanded = next;
      })
      if (this.isProcessExpanded(row)) {
        ForEach(row.entries ?? [], (entry: ProcessEntry) => {
          if (entry.message) {
            Column() { MarkdownView({ source: entry.message.content }) }
              .width('100%').alignItems(HorizontalAlign.Start)
          } else if (entry.observation) {
            Column({ space: 4 }) {
              Text(`${entry.observation.agent_name} · ${entry.observation.tool_name} · ${entry.observation.kind === 'tool_started' ? '调用' : entry.observation.success ? '完成' : '失败'}`)
                .fontSize(13).fontColor(Theme.secondary)
              if (entry.observation.input_summary) { Text(entry.observation.input_summary).fontSize(12).fontColor(Theme.secondary).copyOption(CopyOptions.LocalDevice) }
              if (entry.observation.output) { Text(entry.observation.output).fontSize(12).fontColor(Theme.secondary).fontFamily('monospace').copyOption(CopyOptions.LocalDevice) }
              if (entry.observation.truncated) { Text('输出已截断').caption() }
              if (entry.observation.late_result) { Text('任务结束后的返回').caption() }
            }.width('100%').alignItems(HorizontalAlign.Start)
          }
        }, (entry: ProcessEntry) => `${entry.sequence}`)
      }
    }.width('100%').alignItems(HorizontalAlign.Start).padding({ left: 46 })
  }
  @Builder private planStep(todo: TodoDto, index: number, last: boolean) {
    Row({ space: 16 }) {
      Column() {
        Stack() {
          if (todo.status === 'completed') {
            Circle().width(24).height(24).fill(Theme.accent)
            SymbolGlyph($r('sys.symbol.checkmark')).fontSize(12).fontColor([Color.White])
          } else if (todo.status === 'in_progress') {
            Circle().width(24).height(24).fill(Theme.surface).stroke(Theme.accent).strokeWidth(1.5)
            Circle().width(10).height(10).fill(Theme.accent)
          } else {
            Circle().width(24).height(24).fill(Theme.surface).stroke(Theme.faint).strokeWidth(1)
          }
        }.width(24).height(24)
        if (!last) { Column().width(1).height(26).backgroundColor(Theme.line).margin({ top: 4 }) }
      }
      Text(`${index + 1}`.padStart(2, '0')).fontSize(14).fontColor(Theme.secondary).margin({ top: 3 })
      Text(todo.content).fontSize(15).fontColor(todo.status === 'pending' ? Theme.secondary : Theme.text)
        .layoutWeight(1).margin({ top: 2 })
    }.width('100%').alignItems(VerticalAlign.Top)
  }
  @Builder private interactions() {
    ForEach(this.approvals(), (request: ApprovalDto) => {
      Column({ space: 10 }) {
        Row({ space: 8 }) {
          Circle().width(8).height(8).fill(Theme.warn)
          Text(`${request.agent_name} 请求使用 ${request.tool_name}`).fontSize(15).fontColor(Theme.text).fontWeight(FontWeight.Medium)
        }
        if (request.context) { Text(request.context).fontSize(13).fontColor(Theme.secondary) }
        if (request.input_json) {
          Text(request.input_json).fontSize(12).fontFamily('monospace').fontColor('#4A4840').maxLines(8)
            .textOverflow({ overflow: TextOverflow.Ellipsis }).copyOption(CopyOptions.LocalDevice)
            .width('100%').padding(10).backgroundColor(Theme.page).borderRadius(8)
        }
        if (request.claimed) { Text('已提交，等待运行时校验').caption() }
        else {
          TextInput({ placeholder: '附加说明（可选）', text: this.feedback(request) }).height(36).fontSize(13).backgroundColor(Theme.page)
            .onChange((value: string) => { this.editFeedback(request, value); })
          Flex({ wrap: FlexWrap.Wrap }) {
            ForEach(request.options, (option: ApprovalOptionDto, index: number) => {
              Button(option.label).ghost()
                .fontColor(index === 0 ? Color.White : Theme.text).backgroundColor(index === 0 ? Theme.accent : Color.Transparent)
                .border({ width: index === 0 ? 0 : 1, color: Theme.line, radius: 16 }).margin({ right: 8, bottom: 8 })
                .enabled(this.connected && !this.busy && !this.hasPending && this.connection.supports('approvals.respond'))
                .onClick(() => { this.approve(request, option.id); })
            })
          }
        }
      }.card().backgroundColor(Theme.warnSoft).border({ width: 0, radius: 14 })
    }, (request: ApprovalDto) => `${request.request_id}:${request.version}:${request.claimed}`)
    ForEach(this.questions(), (question: QuestionDto) => {
      Column({ space: 10 }) {
        Text('Agent 想确认').caption()
        Text(question.question).fontSize(15).lineHeight(24).fontColor(Theme.text).copyOption(CopyOptions.LocalDevice)
        if (question.claimed) { Text('已提交回答，等待处理').caption() }
        else {
          Row({ space: 8 }) {
            TextInput({ text: this.questionDraft(question), placeholder: '你的回答' }).height(38).fontSize(14).layoutWeight(1)
              .backgroundColor(Theme.page).onChange((value: string) => { this.editAnswer(question, value); })
            Button('回答').primary().height(38)
              .enabled(this.connected && !this.busy && !this.hasPending && this.connection.supports('questions.answer'))
              .onClick(() => { this.answerQuestion(question); })
          }
        }
      }.card()
    }, (question: QuestionDto) => `${question.task_id}/${question.question_id}:${question.claimed}`)
  }
  @Builder private taskControls() {
    ForEach(this.tasks().filter((task: TaskDto) => (task.can_stop ?? false) || (task.can_finish ?? false)), (task: TaskDto) => {
      Row({ space: 8 }) {
        Text(`${task.name} · ${this.statusLabel(task.status)}`).caption().layoutWeight(1).maxLines(1).textOverflow({ overflow: TextOverflow.Ellipsis })
        if (task.can_stop && this.connection.supports('tasks.stop')) {
          Button('停止本轮').ghost().height(28).fontSize(12).enabled(this.connected && !this.busy && !this.hasPending)
            .onClick(() => { this.control(task, 'stop_task'); })
        }
        if (task.can_finish && this.connection.supports('tasks.finish')) {
          Button('结束任务').ghost().height(28).fontSize(12).enabled(this.connected && !this.busy && !this.hasPending)
            .onClick(() => { this.control(task, 'finish_task'); })
        }
      }.width('100%')
    }, (task: TaskDto) => JSON.stringify(task))
  }
  @Builder private emptyChat() {
    Column({ space: 12 }) {
      Text('有什么想交给 Agent 的？').heading(30)
      Text('描述目标，Agent 会拆解计划、调用工具，并在需要时向你确认。').fontSize(14).fontColor(Theme.secondary)
    }.alignItems(HorizontalAlign.Start).width('100%').margin({ top: 96 })
  }
  @Builder private conversation() {
    Column() {
      this.header()
      Scroll(this.conversationScroller) {
        Column({ space: 26 }) {
          if (!this.selected || !this.current()) { this.emptyChat() }
          else {
            Text(this.current()?.title ?? '').heading(28).maxLines(2).textOverflow({ overflow: TextOverflow.Ellipsis }).width('100%').margin({ top: 4 })
            if (this.current()?.history_truncated) { Text('更早的消息已超出保留范围').caption() }
            if (this.current()?.observations_truncated) { Text('更早的执行记录已超出保留范围').caption() }
            ForEach(this.current() ? conversationRows(this.current() as SessionDto) : [], (row: ConversationRow) => {
              if (row.message) { this.message(row.message) }
              else { this.process(row) }
            }, (row: ConversationRow) => `${row.key}:${row.complete}:${row.entries?.length ?? 0}`)
            if (this.todos().length > 0) {
              Column({ space: 14 }) {
                Divider().color(Theme.line)
                Text('执行计划').heading(22).margin({ top: 6, bottom: 4 })
                ForEach(this.todos(), (todo: TodoDto, index: number) => { this.planStep(todo, index, index === this.todos().length - 1) },
                  (todo: TodoDto, index: number) => `${index}:${todo.status}:${todo.content}`)
              }.width('100%').alignItems(HorizontalAlign.Start)
            }
            this.interactions()
            this.taskControls()
          }
        }.width('100%').constraintSize({ maxWidth: Theme.contentMax }).padding({ left: 24, right: 24, top: 8, bottom: 24 })
      }.layoutWeight(1).width('100%').align(Alignment.Top).scrollBar(BarState.Off).edgeEffect(EdgeEffect.Spring)
      this.composer()
    }.width('100%').height('100%')
  }
  @Builder private composer() {
    Column({ space: 8 }) {
      if (this.hasPending) {
        Row({ space: 8 }) {
          Text('有命令结果未确认').fontSize(12).fontColor(Theme.warn).layoutWeight(1)
          Text('查状态').fontSize(12).fontColor(Theme.accent).enabled(this.connected).onClick(() => { this.queryPending(); })
          Text('重试').fontSize(12).fontColor(Theme.accent).enabled(this.connected && !this.busy).onClick(() => { if (this.pending) { this.submit(this.pending); } })
          Text('放弃').fontSize(12).fontColor(Theme.secondary).enabled(!this.busy)
            .onClick(() => { this.pending = undefined; this.hasPending = false; this.save('pending', ''); })
        }.width('100%').padding({ left: 16, right: 16 })
      }
      if (this.pendingUpload) {
        Row({ space: 8 }) {
          Text(`上传未完成：${this.pendingUpload.name}`).fontSize(12).fontColor(Theme.warn).layoutWeight(1).maxLines(1)
          Text('重试').fontSize(12).fontColor(Theme.accent).onClick(() => { this.uploadFile(true); })
          Text('清除').fontSize(12).fontColor(Theme.secondary).onClick(() => { this.clearUpload(); })
        }.width('100%').padding({ left: 16, right: 16 })
      }
      if (this.attachments.length > 0) {
        Row({ space: 8 }) {
          SymbolGlyph($r('sys.symbol.paperclip')).fontSize(14).fontColor([Theme.secondary])
          Text(this.attachments.map((id: string) => this.files.find((file: ClientFileDto) => file.id === id)?.name ?? '附件').join('、'))
            .fontSize(12).fontColor(Theme.secondary).layoutWeight(1).maxLines(1).textOverflow({ overflow: TextOverflow.Ellipsis })
          Text('移除').fontSize(12).fontColor(Theme.accent).enabled(!this.busy && !this.hasPending)
            .onClick(() => { this.attachments = []; this.save(`attachments:${this.selected}`, '[]'); })
        }.width('100%').padding({ left: 16, right: 16 })
      }
      if (this.status) { Text(this.status).caption().width('100%').padding({ left: 16, right: 16 }).maxLines(3) }
      Row({ space: 4 }) {
        Button() { SymbolGlyph($r('sys.symbol.paperclip')).fontSize(19).fontColor([Theme.secondary]) }
          .icon().enabled(this.connected && !this.busy && !this.pendingUpload && this.connection.supports('files.transfer'))
          .onClick(() => { this.uploadFile(false); })
        TextArea({ placeholder: this.current() ? '继续补充想法 …' : '描述你的目标 …', text: this.draft })
          .layoutWeight(1).fontSize(15).backgroundColor(Color.Transparent).constraintSize({ minHeight: 40, maxHeight: 160 })
          .placeholderColor(Theme.faint).padding({ left: 6, right: 6, top: 10, bottom: 10 })
          .onChange((value: string) => { this.draft = value; this.save(`draft:${this.selected}`, value); })
        Button() { SymbolGlyph($r('sys.symbol.arrow_up')).fontSize(18).fontColor([Color.White]) }
          .type(ButtonType.Circle).width(40).height(40).backgroundColor(Theme.accent)
          .enabled(this.connected && !this.busy && !this.hasPending && (this.draft.trim().length > 0 || this.attachments.length > 0) &&
            this.connection.supports('sessions.text'))
          .onClick(() => { this.send(); })
      }.width('100%').alignItems(VerticalAlign.Bottom).padding({ left: 8, right: 8, top: 4, bottom: 4 })
        .backgroundColor(Theme.surface).borderRadius(26).border({ width: 1, color: Theme.line })
        .shadow({ radius: 16, color: '#0F000000', offsetY: 4 })
    }.width('100%').constraintSize({ maxWidth: Theme.contentMax + 48 }).padding({ left: 24, right: 24, bottom: 18 + this.bottom(), top: 6 })
  }

  // ---------- 右侧面板：产物与执行记录（远端资源预览留待第二阶段） ----------
  @Builder private panelTabButton(label: string, tab: string) {
    Text(label).fontSize(14).fontColor(this.panelTab === tab ? Theme.text : Theme.secondary)
      .fontWeight(this.panelTab === tab ? FontWeight.Medium : FontWeight.Normal)
      .padding({ left: 12, right: 12, top: 6, bottom: 6 }).borderRadius(14)
      .backgroundColor(this.panelTab === tab ? Theme.selected : Color.Transparent)
      .onClick(() => { this.panelTab = tab; })
  }
  @Builder private panel() {
    Column() {
      Row({ space: 4 }) {
        this.panelTabButton(`产物 ${this.sessionFiles().length + this.sessionLinks().length || ''}`, 'artifacts')
        this.panelTabButton('执行记录', 'execution')
        Blank()
        Button() { SymbolGlyph($r('sys.symbol.xmark')).fontSize(18).fontColor([Theme.secondary]) }
          .icon().onClick(() => { this.setPanel(false); })
      }.width('100%').height(52 + this.top()).padding({ left: 12, right: 8, top: this.top() })
      Scroll() {
        Column({ space: 12 }) {
          if (this.panelTab === 'artifacts') { this.artifacts() } else { this.execution() }
        }.width('100%').padding({ left: 16, right: 16, bottom: 24 + this.bottom() })
      }.layoutWeight(1).align(Alignment.Top).scrollBar(BarState.Off)
        .visibility(this.panelTab === 'artifacts' && this.viewing ? Visibility.None : Visibility.Visible)
      if (this.panelTab === 'artifacts' && this.viewing) {
        // 每个产物独立创建查看器，Web 与视频控制器不跨产物复用。
        Column() {
          ForEach([this.viewing as ArtifactRef], (ref: ArtifactRef) => {
            ArtifactViewer({ target: ref, connection: this.connection, onBack: () => { this.closeArtifact(); },
              onSave: (file: ClientFileDto) => { this.downloadFile(file); } }).width('100%').height('100%')
          }, (ref: ArtifactRef) => ref.key)
        }.width('100%').layoutWeight(1).padding({ bottom: this.bottom() })
      }
    }.width('100%').height('100%').backgroundColor(Theme.surface).border({ width: { left: 1 }, color: Theme.line })
  }
  @Builder private artifactRow(ref: ArtifactRef) {
    Row({ space: 12 }) {
      Stack() { SymbolGlyph(artifactIcon(ref.kind)).fontSize(17).fontColor([Theme.accent]) }
        .width(36).height(36).borderRadius(10).backgroundColor(Theme.accentSoft)
      Column({ space: 3 }) {
        Text(ref.title).fontSize(14).fontColor(Theme.text).maxLines(1).textOverflow({ overflow: TextOverflow.Ellipsis })
        Text(ref.detail).caption()
      }.alignItems(HorizontalAlign.Start).layoutWeight(1)
      if (ref.file) {
        Text('附加').fontSize(12).fontColor(Theme.accent).enabled(!this.hasPending && !this.busy)
          .onClick(() => { if (ref.file) { this.attachFile(ref.file); } })
      }
      SymbolGlyph($r('sys.symbol.chevron_right')).fontSize(14).fontColor([Theme.faint])
    }.width('100%').padding({ top: 10, bottom: 10 }).border({ width: { bottom: 1 }, color: Theme.line })
      .onClick(() => { this.openArtifact(ref); })
  }
  @Builder private artifacts() {
    if (this.sessionFiles().length > 0) {
      Text('文件').caption().width('100%').margin({ top: 4 })
      ForEach(this.sessionFiles().slice().reverse(), (file: ClientFileDto) => { this.artifactRow(fileArtifact(file)) }, (file: ClientFileDto) => file.id)
    }
    if (this.sessionLinks().length > 0) {
      Text('链接').caption().width('100%').margin({ top: 12 })
      ForEach(this.sessionLinks(), (ref: ArtifactRef) => { this.artifactRow(ref) }, (ref: ArtifactRef) => ref.key)
    }
    if (this.sessionFiles().length + this.sessionLinks().length === 0) {
      Text('这个任务还没有产物。Agent 发送的文件、回复中的链接和你上传的附件会出现在这里。').caption().margin({ top: 12 })
    }
  }
  @Builder private execution() {
    if (this.current()?.observations_truncated) { Text('更早的执行记录已超出保留范围').caption() }
    ForEach((this.current()?.observations ?? []).slice().reverse(), (item: ObservationDto) => {
      Column({ space: 4 }) {
        Row({ space: 8 }) {
          Circle().width(6).height(6).fill(item.kind === 'tool_started' ? Theme.faint : item.kind === 'permission_audit' ? Theme.warn :
            item.success ? Theme.accent : Theme.danger)
          Text(`${item.tool_name}`).fontSize(13).fontColor(Theme.text).fontWeight(FontWeight.Medium)
          Text(item.agent_name).caption()
        }
        Text(item.kind === 'permission_audit' ? `权限：${item.action ?? ''} ${item.context ?? ''}` : item.kind === 'tool_started' ? '开始调用' :
          item.success ? '返回成功' : '返回错误').caption()
        if (item.input_summary) { Text(item.input_summary).fontSize(12).fontColor('#4A4840').maxLines(4).textOverflow({ overflow: TextOverflow.Ellipsis }) }
        if (item.output) {
          Text(item.output).fontSize(12).fontFamily('monospace').fontColor('#4A4840').maxLines(10).textOverflow({ overflow: TextOverflow.Ellipsis })
            .copyOption(CopyOptions.LocalDevice).width('100%').padding(8).backgroundColor(Theme.page).borderRadius(6)
        }
        if (item.truncated) { Text('输出已截断').fontSize(11).fontColor(Theme.warn) }
        if (item.late_result) { Text('任务结束后的返回，不改变任务终态').fontSize(11).fontColor(Theme.warn) }
      }.width('100%').alignItems(HorizontalAlign.Start).padding({ top: 10, bottom: 10 }).border({ width: { bottom: 1 }, color: Theme.line })
    })
    if ((this.current()?.observations ?? []).length === 0) { Text('还没有工具调用记录').caption().margin({ top: 12 }) }
  }

  // ---------- 其他页面 ----------
  @Builder private otherPage() {
    Column() {
      this.header()
      Scroll() {
        Column({ space: 16 }) {
          Text(this.view === 'automation' ? '自动化' : this.view === 'inbox' ? '通知' : '设备与连接').heading(32).width('100%')
          if (this.view === 'automation') { Text('定时运行的任务，结果回到所选任务中。').fontSize(14).fontColor(Theme.secondary).width('100%') }
          if (this.view === 'inbox') {
            Text(this.connection.capabilities?.meow_enabled ? '重要事件由服务端经 MeoW 推送。' : '未配置 MeoW 推送，待办仍会在任务中显示。')
              .fontSize(14).fontColor(Theme.secondary).width('100%')
          }
          if (this.status) { Text(this.status).caption().width('100%') }
          if (this.view === 'automation') { this.automationBody() }
          else if (this.view === 'inbox') { this.inboxBody() }
          else { this.managementBody() }
        }.width('100%').constraintSize({ maxWidth: Theme.contentMax }).padding({ left: 24, right: 24, bottom: 32 + this.bottom() })
      }.layoutWeight(1).width('100%').align(Alignment.Top).scrollBar(BarState.Off)
    }.width('100%').height('100%')
  }
  @Builder private automationBody() {
    Row() {
      Text(`时区 ${this.automation.timezone || '—'}`).caption().layoutWeight(1)
      Button('刷新').ghost().enabled(this.connected && !this.busy && !this.hasPending).onClick(() => { this.manageSchedule({ kind: 'list' }); })
    }.width('100%')
    if (this.automation.stale) { Text('当前显示缓存，请刷新后操作').fontSize(12).fontColor(Theme.warn) }
    ForEach(this.automation.schedules.concat(this.automation.archived ?? []), (plan: ScheduleDto) => {
      Column({ space: 6 }) {
        Row({ space: 8 }) {
          Text(plan.name).fontSize(16).fontColor(Theme.text).fontWeight(FontWeight.Medium).layoutWeight(1)
          if (!plan.archived) {
            Toggle({ type: ToggleType.Switch, isOn: plan.enabled }).selectedColor(Theme.accent)
              .enabled(this.connected && !this.busy && !this.hasPending && !this.automation.stale)
              .onChange(() => { this.togglePlan(plan); })
          } else { Text('已归档').caption() }
        }.width('100%')
        Text(`${plan.schedule}${plan.next_trigger ? ` · 下次 ${plan.next_trigger}` : ''}`).caption().copyOption(CopyOptions.LocalDevice)
        Text(plan.content).fontSize(14).fontColor('#4A4840').maxLines(3).textOverflow({ overflow: TextOverflow.Ellipsis })
        if (plan.health.alerting) { Text(`连续失败 ${plan.health.consecutive_failures} 次`).fontSize(12).fontColor(Theme.danger) }
        Row({ space: 16 }) {
          if (!plan.archived) {
            Text('编辑').fontSize(13).fontColor(Theme.accent).enabled(!this.busy && !this.hasPending && !this.automation.stale).onClick(() => { this.editPlan(plan); })
            Text('删除').fontSize(13).fontColor(Theme.danger).enabled(this.connected && !this.busy && !this.hasPending && !this.automation.stale)
              .onClick(() => { this.manageSchedule({ kind: 'delete', schedule_id: plan.id, expected_revision: plan.revision }); })
          }
          Text('运行记录').fontSize(13).fontColor(Theme.accent).enabled(this.connected && !this.busy && !this.hasPending)
            .onClick(() => { this.manageSchedule({ kind: 'history', schedule_id: plan.id }); })
        }.margin({ top: 4 })
      }.card()
    }, (plan: ScheduleDto) => JSON.stringify(plan))
    if (this.automation.schedules.length === 0) { Text('还没有定时任务').caption() }
    if (this.automation.archive_truncated) { Text('部分计划超出归档展示上限').caption() }
    if (this.automation.history) {
      Text('最近运行').heading(20).margin({ top: 8 })
      ForEach(this.automation.history.records, (run: RunDto) => {
        Column({ space: 4 }) {
          Text(`${run.fired_at} · ${run.outcome === 'notified' ? '通道已接受' : run.outcome}`).caption()
          Text(run.result_summary).fontSize(14).fontColor(Theme.text)
          if (run.error) { Text(run.error).fontSize(12).fontColor(Theme.danger) }
        }.width('100%').alignItems(HorizontalAlign.Start).padding({ top: 8, bottom: 8 }).border({ width: { bottom: 1 }, color: Theme.line })
      })
      if (this.automation.history.records.length === 0) { Text('还没有运行记录').caption() }
    }
    Column({ space: 10 }) {
      Text(this.editPlanId ? '编辑计划' : '新建计划').heading(20)
      TextInput({ placeholder: '名称', text: this.planName }).backgroundColor(Theme.page).onChange((value: string) => { this.planName = value; })
      TextArea({ placeholder: '每次运行时交给 Agent 的内容', text: this.planContent }).height(96).backgroundColor(Theme.page)
        .onChange((value: string) => { this.planContent = value; })
      TextInput({ placeholder: 'cron:0 8 * * * 或 once:2026-11-01T08:00:00+08:00', text: this.planExpression }).backgroundColor(Theme.page)
        .onChange((value: string) => { this.planExpression = value; })
      Row({ space: 12 }) {
        Text('启用').fontSize(14).fontColor(Theme.text)
        Toggle({ type: ToggleType.Switch, isOn: this.planEnabled }).selectedColor(Theme.accent).onChange((value: boolean) => { this.planEnabled = value; })
        Blank()
        Text(this.planNotify === 'reply' ? '结果自动回复' : '仅显式通知').fontSize(13).fontColor(Theme.accent)
          .onClick(() => { this.planNotify = this.planNotify === 'reply' ? 'explicit' : 'reply'; })
      }.width('100%')
      Row({ space: 10 }) {
        Button('保存').primary().enabled(this.connected && !this.busy && !this.hasPending && this.connection.supports('schedules.manage'))
          .onClick(() => { this.savePlan(); })
        if (this.editPlanId) { Button('取消').ghost().height(40).onClick(() => { this.resetPlan(); }) }
      }
    }.card().margin({ top: 8 })
  }
  @Builder private inboxBody() {
    if (this.notificationOverflow > 0) { Text(`通知队列容量不足：${this.notificationOverflow} 条未入队`).fontSize(12).fontColor(Theme.danger) }
    if (this.notificationsTruncated) { Text('仅显示最近 200 条').caption() }
    ForEach(this.notifications, (notification: NotificationDto) => {
      Column({ space: 6 }) {
        Row({ space: 8 }) {
          if (!notification.read_at) { Circle().width(7).height(7).fill(Theme.accent) }
          Text(notification.title).fontSize(15).fontColor(Theme.text).fontWeight(FontWeight.Medium).layoutWeight(1)
        }.width('100%')
        Text(notification.message).fontSize(14).fontColor('#4A4840')
        Text(`${this.notificationState(notification.state)}${notification.result_code ? ` · ${notification.result_code}` : ''}`).caption()
        Row({ space: 16 }) {
          Text('查看任务').fontSize(13).fontColor(Theme.accent).onClick(() => { this.select(notification.session_id); })
          if (!notification.read_at) {
            Text('标记已读').fontSize(13).fontColor(Theme.accent).enabled(this.connected && !this.busy).onClick(() => { this.notificationAction(notification, false); })
          }
          if (notification.state === 'failed' || notification.state === 'unknown') {
            Text(notification.state === 'unknown' ? '重试（可能重复）' : '重试').fontSize(13).fontColor(Theme.accent)
              .enabled(this.connected && !this.busy).onClick(() => { this.notificationAction(notification, true); })
          }
        }.margin({ top: 2 })
      }.width('100%').alignItems(HorizontalAlign.Start).padding({ top: 12, bottom: 12 }).border({ width: { bottom: 1 }, color: Theme.line })
    }, (notification: NotificationDto) => JSON.stringify(notification))
    if (this.notifications.length === 0) { Text('暂无通知').caption() }
  }
  @Builder private managementBody() {
    Column({ space: 8 }) {
      Row({ space: 8 }) {
        Circle().width(8).height(8).fill(this.connected ? Theme.accent : Theme.faint)
        Text(this.connected ? `已连接 ${this.connection.capabilities?.instance_name ?? ''}` : '未连接').fontSize(15).fontColor(Theme.text).layoutWeight(1)
      }
      Text(`地址 ${this.preferred || '—'}${this.addresses.length > 1 ? `（共 ${this.addresses.length} 个候选）` : ''}`).caption()
      Text(this.fingerprint ? `实例证书指纹 ${this.fingerprint.substring(0, 16)}…` : '使用系统信任的证书').caption()
      Row({ space: 10 }) {
        Button('重新连接').ghost().enabled(!this.busy).onClick(() => { this.connect(''); })
        Button('重新配对').ghost().enabled(!this.busy).onClick(() => { this.unpair(); })
      }.margin({ top: 6 })
    }.card()
    if (this.diagnostics) {
      Column({ space: 6 }) {
        Text('诊断').heading(18)
        Text(`Harness ${this.diagnostics.version} · 存储${this.diagnostics.storage_available ? '可用' : '不可用'}`).fontSize(14).fontColor(Theme.text)
        Text(`${this.diagnostics.sessions} 个任务 · ${this.diagnostics.pending_approvals} 项审批 · ${this.diagnostics.pending_questions} 个问题`).caption()
        Text(`同步游标 ${this.diagnostics.cursor} · 保留 ${this.diagnostics.event_retention} 条事件 · 重启后未知命令 ${this.diagnostics.unknown_commands}`).caption()
        Text(`实例 ${this.diagnostics.instance_id}`).caption().copyOption(CopyOptions.LocalDevice)
      }.card()
    }
    Text('已配对设备').heading(18).margin({ top: 8 })
    ForEach(this.devices, (device: DeviceDto) => {
      Row({ space: 10 }) {
        Column({ space: 3 }) {
          Text(device.name).fontSize(15).fontColor(Theme.text)
          Text(device.revoked ? '已撤销' : `有效至 ${new Date(device.expires_at * 1000).toLocaleDateString()}`).caption()
        }.alignItems(HorizontalAlign.Start).layoutWeight(1)
        if (!device.revoked) {
          Text('撤销').fontSize(13).fontColor(Theme.danger)
            .enabled(this.connected && !this.busy && this.connection.supports('devices.revoke')).onClick(() => { this.revokeDevice(device); })
        }
      }.width('100%').padding({ top: 10, bottom: 10 }).border({ width: { bottom: 1 }, color: Theme.line })
    }, (device: DeviceDto) => JSON.stringify(device))
    Text('撤销当前设备会断开连接，需要重新配对。模型配置、全局 Skills 与共享记忆暂未通过客户端开放。').caption().margin({ top: 8 })
  }

  @Builder private main() {
    Stack() {
      Row() {
        Column() {
          if (this.view === 'chat') { this.conversation() } else { this.otherPage() }
        }.layoutWeight(1).height('100%')
        if (this.panelOpen && this.view === 'chat' && this.current() && this.wide()) { Column() { this.panel() }.width(this.panelWidth()).height('100%').clip(true)
          .transition(TransitionEffect.move(TransitionEdge.END).combine(TransitionEffect.OPACITY)) }
      }.width('100%').height('100%').backgroundColor(Theme.page)
      if (this.panelOpen && this.view === 'chat' && this.current() && !this.wide()) { Column() { this.panel() }.width('100%').height('100%')
        .transition(TransitionEffect.move(TransitionEdge.END)) }
      if (this.sidebarOpen && !this.wide()) {
        Column().width('100%').height('100%').backgroundColor('#33000000').onClick(() => { this.setSidebar(false); })
          .transition(TransitionEffect.OPACITY)
      }
    }.width('100%').height('100%')
  }

  build() {
    Column() {
      if (!this.paired) { this.onboarding() }
      else {
        SideBarContainer(this.wide() ? SideBarContainerType.Embed : SideBarContainerType.Overlay) {
          this.sidebar()
          this.main()
        }.showSideBar(this.sidebarOpen).showControlButton(false).autoHide(false)
          .sideBarWidth(this.wide() ? 272 : 300).minSideBarWidth(this.wide() ? 272 : 300).maxSideBarWidth(this.wide() ? 272 : 300)
          .divider({ strokeWidth: 1, color: Theme.line })
          .onChange((value: boolean) => { this.sidebarOpen = value; })
      }
    }.height('100%').width('100%').backgroundColor(Theme.page)
      .onAreaChange((_old, area) => {
        const width = Number(area.width);
        if (width === this.windowWidth) { return; }
        const wasWide = this.wide();
        this.windowWidth = width;
        // 跨越断点时按新形态重置侧栏：平板默认展开，手机默认收起。
        if (wasWide !== this.wide()) { this.sidebarOpen = this.wide(); }
      })
  }
}
