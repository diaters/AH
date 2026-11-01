import { common } from '@kit.AbilityKit';
import { preferences } from '@kit.ArkData';
import { fileIo, fileUri, picker } from '@kit.CoreFileKit';
import { util } from '@kit.ArkTS';
import { deviceInfo } from '@kit.BasicServicesKit';
import { scanBarcode, scanCore } from '@kit.ScanKit';
import { Connection, CredentialStore, PairingLink, parseLink, trustCa } from '../services/Connection';
import { ApprovalDto, ApprovalOptionDto, AutomationDto, ClientFileDto, CommandDto, DeviceDto, DiagnosticsDto, ExperienceDraftDto, ExperienceDto,
  ExperienceSummaryDto, MessageDto, NotificationDto, ObservationDto, PendingUploadDto, ProgressDto, QuestionDto, RunDto, ScheduleActionDto, ScheduleDto, ScheduleInputDto, SessionDto, TaskDto, TodoDto, formatLocalTime, isDenyOption, observationOutcome } from '../services/Protocol';
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

interface SavedInstance { instance: string; fingerprint: string; addresses: string[]; preferred: string; name: string; }

@Entry
@Component
struct Index {
  @State processExpanded: Record<string, boolean> = {};
  @State instance: string = '';
  @State instances: SavedInstance[] = [];
  @State instanceMenu: boolean = false;
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
  // 经验总结：草稿编辑态按草稿 ID 保存，刷新快照不会冲掉未保存的修改。
  // 能力来自连接对象（非状态），镜像为状态以便侧栏等常驻区域随连接刷新。
  @State experienceEnabled: boolean = false;
  @State summaryOpen: boolean = false;
  @State summaryNote: string = '';
  @State draftTitles: Record<string, string> = {};
  @State draftBodies: Record<string, string> = {};
  @State draftEditing: Record<string, boolean> = {};
  @State draftAsNew: Record<string, boolean> = {};
  // 经验库页面
  @State experiences: ExperienceSummaryDto[] = [];
  @State experienceQuery: string = '';
  @State experienceDetail: ExperienceDto | undefined = undefined;
  @State experienceEditing: boolean = false;
  @State experienceTitle: string = '';
  @State experienceBody: string = '';
  @State experienceDeleting: string = '';
  private conversationScroller: Scroller = new Scroller();
  private editPlanRevision: string = '';
  // 左缘右拉时记录起手坐标（vp）；不在左侧起手则忽略，避免误吞内容区的横向操作。
  private panStartX: number = -1;
  private panStartY: number = -1;
  // 左滑收起时记录起手坐标，与右拉唤出分开，互不干扰。
  private closeStartX: number = -1;
  private closeStartY: number = -1;
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
      this.loadLocalState();
    } catch (_) { this.status = '无法读取本地状态，请检查应用存储'; }
    if (this.paired) { this.connect(''); }
  }
  aboutToDisappear(): void { this.cancelReconnect(); this.connection.close(); }

  // 旧版的单个配对记录作为首个实例保留，旧状态按原实例惰性读取。
  private loadPairing(): void {
    const legacy = this.storage?.getSync('pair.instance', '') as string ?? '';
    try { this.instances = JSON.parse(this.storage?.getSync('instances', '[]') as string ?? '[]') as SavedInstance[]; }
    catch (_) { this.instances = []; }
    if (this.instances.length === 0 && legacy) {
      let addresses: string[] = [];
      try { addresses = JSON.parse(this.storage?.getSync('pair.addresses', '[]') as string ?? '[]') as string[]; } catch (_) {}
      if (addresses.length > 0) {
        this.instances = [{ instance: legacy, fingerprint: this.storage?.getSync('pair.fingerprint', '') as string ?? '',
          addresses: addresses, preferred: this.storage?.getSync('pair.preferred', '') as string ?? '', name: '' }];
        this.save('instances', JSON.stringify(this.instances));
      }
    }
    const active = this.storage?.getSync('active-instance', legacy) as string ?? legacy;
    const saved = this.instances.find((item: SavedInstance) => item.instance === active) ?? this.instances[0];
    if (saved) { this.usePairing(saved); } else { this.paired = false; }
  }
  private usePairing(saved: SavedInstance): void {
    this.instance = saved.instance; this.fingerprint = saved.fingerprint;
    this.addresses = saved.addresses; this.preferred = saved.preferred; this.paired = true;
  }
  private savedInstance(): string { return this.instances.length > 0 ? this.instances[0].instance : ''; }
  private localKey(key: string): string { return `instance:${this.instance}:${key}`; }
  private localRead(key: string, fallback: string): string {
    if (!this.storage) { return fallback; }
    const missing = '__harness_missing_local_state__';
    const value = this.storage.getSync(this.localKey(key), missing) as string;
    if (value !== missing) { return value; }
    const legacy = this.storage.getSync('pair.instance', '') as string;
    return legacy === this.instance ? this.storage.getSync(key, fallback) as string : fallback;
  }
  private loadLocalState(): void {
    this.selected = this.localRead('selected', '');
    this.draft = this.localRead(`draft:${this.selected}`, '');
    try { this.attachments = JSON.parse(this.localRead(`attachments:${this.selected}`, '[]')) as string[]; } catch (_) { this.attachments = []; }
    this.pendingUpload = undefined; this.pending = undefined; this.hasPending = false;
    const upload = this.localRead('pending-upload', '');
    if (upload) { try { this.pendingUpload = JSON.parse(upload) as PendingUploadDto; } catch (_) { this.status = '待上传记录损坏，请检查'; } }
    const pending = this.localRead('pending', '');
    if (pending) { try { this.pending = JSON.parse(pending) as CommandDto; this.hasPending = true; } catch (_) { this.status = '待发送记录损坏，请检查'; } }
  }
  private resetInstanceView(): void {
    this.sessions = []; this.files = []; this.notifications = []; this.notificationsTruncated = false; this.notificationOverflow = 0;
    this.automation = { available: false, schedules: [], timezone: '' }; this.devices = []; this.diagnostics = undefined;
    this.questionDrafts = {}; this.approvalFeedback = {}; this.processExpanded = {}; this.viewing = undefined;
    this.summaryOpen = false; this.summaryNote = ''; this.draftTitles = {}; this.draftBodies = {}; this.draftEditing = {}; this.draftAsNew = {};
    this.experienceEnabled = false; this.experiences = []; this.experienceDetail = undefined; this.experienceEditing = false; this.experienceDeleting = '';
    this.lastCommandId = ''; this.view = 'chat'; this.resetPlan();
  }
  private switchInstance(saved: SavedInstance): void {
    if (saved.instance === this.instance) { this.instanceMenu = false; return; }
    if (this.busy || this.refreshing) { this.status = '正在同步，请稍后再切换'; return; }
    this.cancelReconnect(); this.connection.close(); this.connected = false; this.resetInstanceView();
    this.usePairing(saved); this.save('active-instance', saved.instance); this.loadLocalState();
    this.instanceMenu = false; this.connect('');
  }
  private top(): number { return this.getUIContext().px2vp(this.safeTop); }
  private bottom(): number { return this.getUIContext().px2vp(this.safeBottom); }
  // 侧栏与右侧面板的显隐统一走同一曲线，内嵌布局的宽度变化随之过渡。
  private setSidebar(open: boolean): void {
    this.getUIContext().animateTo({ duration: 300, curve: Curve.Friction }, () => { this.sidebarOpen = open; });
  }
  private setPanel(open: boolean): void {
    this.getUIContext().animateTo({ duration: 300, curve: Curve.Friction }, () => { this.panelOpen = open; });
  }
  // 左滑收起的共用判定：侧栏自身与遮罩都走这里。
  // 要求水平位移 ≥48vp 且横向明显占优，避免与列表纵向滚动、按钮点击冲突。
  private swipeClose(event: TouchEvent): void {
    if (event.type === TouchType.Down) {
      this.closeStartX = event.touches.length > 0 ? event.touches[0].x : -1;
      this.closeStartY = event.touches.length > 0 ? event.touches[0].y : -1;
      return;
    }
    if (event.type !== TouchType.Move || event.touches.length === 0) { return; }
    if (this.closeStartX < 0) { return; }
    const dx = event.touches[0].x - this.closeStartX;
    const dy = event.touches[0].y - this.closeStartY;
    if (dx <= -48 && Math.abs(dx) >= Math.abs(dy) * 1.5) {
      this.closeStartX = -1;
      this.setSidebar(false);
    }
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
      const local = key === 'selected' || key === 'pending' || key === 'pending-instance' || key === 'pending-upload' ||
        key.startsWith('draft:') || key.startsWith('attachments:') || key.startsWith('answer:') || key.startsWith('approval:');
      this.storage.putSync(local ? this.localKey(key) : key, value); this.storage.flushSync(); return true;
    } catch (_) { this.status = '无法保存本地状态，尚未发送新命令'; return false; }
  }
  private select(id: string): void {
    this.viewing = undefined;
    this.selected = id; this.save('selected', id); this.view = 'chat';
    if (!this.wide() && this.sidebarOpen) { this.setSidebar(false); }
    try { this.draft = this.localRead(`draft:${id}`, ''); }
    catch (_) { this.status = '无法读取草稿'; this.draft = ''; }
    try { this.attachments = JSON.parse(this.localRead(`attachments:${id}`, '[]')) as string[]; }
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
    try { return this.localRead(key, ''); } catch (_) { return ''; }
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
    this.resetInstanceView();
    this.instance = link.instance; this.fingerprint = link.fingerprint; this.addresses = link.addresses; this.preferred = '';
    this.loadLocalState();
    await this.connect(link.code);
    if (!this.connected) { return; }
    const saved: SavedInstance = { instance: link.instance, fingerprint: link.fingerprint, addresses: link.addresses,
      preferred: this.preferred, name: this.connection.capabilities?.instance_name ?? '' };
    this.instances = this.instances.filter((item: SavedInstance) => item.instance !== link.instance).concat([saved]);
    this.save('instances', JSON.stringify(this.instances)); this.save('active-instance', link.instance);
    this.paired = true; this.linkText = ''; this.sidebarOpen = this.wide();
  }
  private unpair(): void {
    if (this.busy || this.refreshing) { this.status = '正在同步，请稍后再操作'; return; }
    this.cancelReconnect(); this.connection.close(); this.connected = false; this.paired = false; this.status = ''; this.instanceMenu = false;
  }

  // 依次尝试配对链接中的地址：先按指纹信任实例 CA，再核对实例身份，之后才发送设备凭证。
  private async connect(code: string): Promise<void> {
    if (this.busy) { return; }
    this.busy = true;
    this.cancelReconnect();
    this.connected = false;
    this.status = code ? '正在配对…' : '正在连接…';
    try {
      if (this.hasPending && this.localRead('pending-instance', '') !== this.instance) {
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
      this.preferred = chosen;
      const saved = this.instances.find((item: SavedInstance) => item.instance === this.instance);
      if (saved && !code) {
        saved.preferred = chosen; saved.name = this.connection.capabilities?.instance_name ?? saved.name;
        this.instances = this.instances.slice(); this.save('instances', JSON.stringify(this.instances));
      }
      const snapshot = await this.connection.snapshot();
      if (!this.foreground) { this.connection.close(); return; }
      this.applySnapshot(snapshot.sessions, snapshot.automation, snapshot.notifications, snapshot.notifications_truncated, snapshot.notification_overflow, snapshot.files);
      this.connected = true;
      this.experienceEnabled = this.connection.supports('experiences.manage');
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
    if (!this.pending || this.busy) { return; }
    this.busy = true;
    try { const receipt = await this.connection.status(this.pending.command_id); this.status = `服务器命令状态：${receipt.status} ${receipt.result_code ?? ''}`; }
    catch (error) { this.status = (error as Error).message; }
    finally { this.busy = false; }
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
    try { return this.localRead(key, ''); } catch (_) { return ''; }
  }
  private editFeedback(request: ApprovalDto, value: string): void {
    const key = `approval:${this.selected}:${request.request_id}`;
    const next: Record<string, string> = {};
    for (const existing of Object.keys(this.approvalFeedback)) { next[existing] = this.approvalFeedback[existing]; }
    next[key] = value; this.approvalFeedback = next; this.save(key, value);
  }
  private hasDenyOption(request: ApprovalDto): boolean {
    return request.options.some((option: ApprovalOptionDto) => isDenyOption(option.id));
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
      request_id: request.request_id, expected_version: request.version, option_id: option };
    // 说明只在拒绝时对 Agent 有意义（运行时仅拒绝分支读取 feedback），
    // 允许时不发送，避免把无效输入当作已生效的沟通。
    const note = isDenyOption(option) ? this.feedback(request).trim() : '';
    if (note.length > 0) { command.feedback = note; }
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
    if (view === 'experiences') { this.experienceDetail = undefined; this.experienceEditing = false; this.loadExperiences(); }
  }

  // ---------- 经验总结与经验库 ----------
  private canManageExperience(): boolean { return this.experienceEnabled; }
  private drafts(): ExperienceDraftDto[] {
    const session = this.current();
    return session ? Object.values(session.experience_drafts ?? {}) : [];
  }
  private draftGenerating(): boolean { return this.drafts().some((draft: ExperienceDraftDto) => draft.status === 'generating'); }
  private copyRecord<T>(source: Record<string, T>, key: string, value: T): Record<string, T> {
    const next: Record<string, T> = {};
    for (const existing of Object.keys(source)) { next[existing] = source[existing]; }
    next[key] = value;
    return next;
  }
  private draftTitle(draft: ExperienceDraftDto): string { return this.draftTitles[draft.draft_id] ?? draft.title ?? ''; }
  private draftBody(draft: ExperienceDraftDto): string { return this.draftBodies[draft.draft_id] ?? draft.body ?? ''; }
  private mergeTarget(draft: ExperienceDraftDto): string {
    return draft.merge_target && !(this.draftAsNew[draft.draft_id] ?? false) ? draft.merge_target.id : '';
  }
  private async startSummary(note: string): Promise<void> {
    if (this.busy || !this.selected) { return; } this.busy = true;
    try {
      await this.connection.startExperienceDraft(this.selected, util.generateRandomUUID(), note);
      this.summaryOpen = false; this.summaryNote = ''; this.status = '';
      await this.refresh();
    } catch (error) { this.status = `无法开始总结：${(error as Error).message}`; }
    finally { this.busy = false; }
  }
  private async saveDraft(draft: ExperienceDraftDto): Promise<void> {
    if (this.busy) { return; } this.busy = true;
    try {
      const saved = await this.connection.saveExperienceDraft(this.selected, draft.draft_id, this.draftTitle(draft).trim(),
        this.draftBody(draft).trim(), this.mergeTarget(draft));
      this.status = this.mergeTarget(draft) ? `已更新经验「${saved.title}」` : `已保存到经验库：${saved.title}`;
      await this.refresh();
    } catch (error) { this.status = `保存失败：${(error as Error).message}`; }
    finally { this.busy = false; }
  }
  private async discardDraft(draft: ExperienceDraftDto, retry: boolean): Promise<void> {
    if (this.busy) { return; } this.busy = true;
    try {
      await this.connection.discardExperienceDraft(this.selected, draft.draft_id);
      if (retry) { await this.connection.startExperienceDraft(this.selected, util.generateRandomUUID(), draft.note ?? ''); }
      await this.refresh();
    } catch (error) { this.status = (error as Error).message; }
    finally { this.busy = false; }
  }
  private async loadExperiences(): Promise<void> {
    if (!this.connected || this.busy || !this.canManageExperience()) { return; } this.busy = true;
    try { this.experiences = (await this.connection.experiences()).experiences; }
    catch (error) { this.status = (error as Error).message; }
    finally { this.busy = false; }
  }
  private async openExperience(id: string): Promise<void> {
    if (this.busy) { return; } this.busy = true;
    try {
      this.experienceDetail = await this.connection.experience(id);
      this.experienceEditing = false; this.experienceDeleting = '';
    } catch (error) { this.status = (error as Error).message; }
    finally { this.busy = false; }
  }
  private editExperience(): void {
    if (!this.experienceDetail) { return; }
    this.experienceTitle = this.experienceDetail.title; this.experienceBody = this.experienceDetail.body; this.experienceEditing = true;
  }
  private async saveExperience(): Promise<void> {
    const detail = this.experienceDetail;
    if (!detail || this.busy) { return; } this.busy = true;
    try {
      this.experienceDetail = await this.connection.updateExperience(detail.id, this.experienceTitle.trim(), this.experienceBody.trim(), detail.updated_at);
      this.experienceEditing = false; this.status = '经验已更新';
      this.experiences = (await this.connection.experiences()).experiences;
    } catch (error) {
      const message = (error as Error).message;
      this.status = message.includes('HTTP 409') ? '这条经验已在别处修改，请重新打开后再编辑' : `保存失败：${message}`;
    }
    finally { this.busy = false; }
  }
  private async deleteExperience(id: string): Promise<void> {
    if (this.busy) { return; } this.busy = true;
    try {
      await this.connection.deleteExperience(id);
      this.experienceDetail = undefined; this.experienceDeleting = ''; this.status = '经验已删除';
      this.experiences = (await this.connection.experiences()).experiences;
    } catch (error) { this.status = (error as Error).message; }
    finally { this.busy = false; }
  }
  private filteredExperiences(): ExperienceSummaryDto[] {
    const query = this.experienceQuery.trim();
    if (!query) { return this.experiences; }
    return this.experiences.filter((item: ExperienceSummaryDto) => item.title.includes(query) || item.scenario.includes(query));
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
          Button('返回已保存的连接').ghost().enabled(!this.busy).onClick(() => { this.loadPairing(); this.resetInstanceView(); this.loadLocalState(); if (this.paired) { this.connect(''); } })
        }
        Text('凭证保存在系统安全存储；实例证书按链接中的指纹校验后才被信任。').caption().textAlign(TextAlign.Center).margin({ top: 12 })
      }.width('100%').constraintSize({ maxWidth: 440 }).padding({ left: 28, right: 28, top: 80 + this.top(), bottom: 40 + this.bottom() })
    }.width('100%').height('100%').align(Alignment.Top).backgroundColor(Theme.page)
  }

  // ---------- 侧栏 ----------
  @Builder private navItem(symbol: Resource, label: string, view: string, badge: number) {
    Row({ space: 12 }) {
      SymbolGlyph(symbol).fontSize(18).fontColor([this.view === view ? Theme.text : Theme.secondary])
      Text(label).fontSize(14).fontColor(Theme.text).layoutWeight(1)
      if (badge > 0) { Text(`${badge}`).fontSize(11).fontColor(Color.White).backgroundColor(Theme.accent).borderRadius(8).padding({ left: 6, right: 6, top: 1, bottom: 1 }) }
    }.width('100%').height(38).padding({ left: 12, right: 12 }).borderRadius(9)
      .backgroundColor(this.view === view ? Theme.selected : Color.Transparent)
      .onClick(() => { this.openView(view); })
  }
  // 实例下拉改为浮层：它曾撑在流里，实例一多就把「最近任务」挤到没有高度。
  @Builder private instancePicker() {
    Column({ space: 2 }) {
      ForEach(this.instances, (item: SavedInstance) => {
        Row() {
          Column({ space: 2 }) {
            Text(item.name || '实例').fontSize(14).fontColor(Theme.text).maxLines(1).textOverflow({ overflow: TextOverflow.Ellipsis })
            Text(item.instance.substring(0, 8)).caption()
          }.alignItems(HorizontalAlign.Start).layoutWeight(1)
          if (item.instance === this.instance) { Text('当前').caption() }
        }.width('100%').padding({ left: 12, right: 12, top: 9, bottom: 9 }).borderRadius(8)
          .backgroundColor(item.instance === this.instance ? Theme.selected : Color.Transparent)
          .onClick(() => { this.switchInstance(item); })
      }, (item: SavedInstance) => item.instance)
      Divider().color(Theme.line).margin({ top: 2, bottom: 2 })
      Text('+ 添加实例').fontSize(14).fontColor(Theme.accent).width('100%').padding({ left: 12, top: 10, bottom: 10 })
        .onClick(() => { this.instanceMenu = false; this.unpair(); })
    }.width(248).padding(6)
  }
  // 列表与空态共用同一份过滤结果，避免两处条件漂移。
  private visibleSessions(): SessionDto[] {
    const query = this.sessionSearch.trim();
    return this.sessions
      .filter((session: SessionDto) => !query || session.title.includes(query) ||
        session.messages.some((message: MessageDto) => message.content.includes(query)))
      .sort((a: SessionDto, b: SessionDto) => (b.updated_at ?? 0) - (a.updated_at ?? 0));
  }
  @Builder private sidebar() {
    Column() {
      Row() {
        Text('Harness').heading(22).layoutWeight(1)
        Button() { SymbolGlyph($r('sys.symbol.close_sidebar')).fontSize(20).fontColor([Theme.secondary]) }
          .icon().onClick(() => { this.setSidebar(false); })
      }.width('100%').margin({ top: 6, bottom: 12 })
      Row({ space: 8 }) {
        Circle().width(8).height(8).fill(this.connected ? Theme.accent : Theme.faint)
        Text(this.instances.find((item: SavedInstance) => item.instance === this.instance)?.name || this.instance.substring(0, 8))
          .fontSize(14).fontColor(Theme.text).layoutWeight(1).maxLines(1).textOverflow({ overflow: TextOverflow.Ellipsis })
        SymbolGlyph($r('sys.symbol.chevron_down')).fontSize(14).fontColor([Theme.secondary])
          .rotate({ angle: this.instanceMenu ? 180 : 0 })
      }.width('100%').height(40).padding({ left: 12, right: 12 }).borderRadius(10).backgroundColor(Theme.surface)
        .onClick(() => { this.instanceMenu = !this.instanceMenu; })
        .bindPopup(this.instanceMenu, {
          builder: this.instancePicker,
          placement: Placement.Bottom,
          popupColor: Theme.surface,
          enableArrow: false,
          radius: 12,
          onStateChange: (event) => { if (!event.isVisible) { this.instanceMenu = false; } }
        })
      Button() {
        Row({ space: 8 }) {
          SymbolGlyph($r('sys.symbol.plus')).fontSize(16).fontColor([Theme.text])
          Text('新任务').fontSize(15).fontColor(Theme.text)
        }
      }.ghost().height(40).width('100%').margin({ top: 8 }).border({ width: 1, color: Theme.line, radius: 10 }).onClick(() => { this.newTask(); })
      Column({ space: 2 }) {
        this.navItem($r('sys.symbol.house'), '工作台', 'chat', this.approvals().length + this.questions().length)
        this.navItem($r('sys.symbol.clock'), '自动化', 'automation', 0)
        this.navItem($r('sys.symbol.bell'), '通知', 'inbox', this.unread())
        if (this.canManageExperience()) { this.navItem($r('sys.symbol.book'), '经验库', 'experiences', 0) }
        this.navItem($r('sys.symbol.gearshape'), '设备与连接', 'management', 0)
      }.margin({ top: 8 })
      Divider().color(Theme.line).margin({ top: 10, bottom: 8 })
      // 搜索占用「最近任务」这一行本身，不再另起一行把列表往下推。
      Row({ space: 6 }) {
        if (this.searching) {
          TextInput({ placeholder: '搜索任务与消息', text: this.sessionSearch }).height(32).fontSize(13).layoutWeight(1)
            .backgroundColor(Theme.surface).onChange((value: string) => { this.sessionSearch = value; })
          Text('取消').fontSize(12).fontColor(Theme.accent).onClick(() => { this.searching = false; this.sessionSearch = ''; })
        } else {
          Text('最近任务').caption().layoutWeight(1)
          Button() { SymbolGlyph($r('sys.symbol.magnifyingglass')).fontSize(16).fontColor([Theme.secondary]) }
            .icon().width(28).height(28).onClick(() => { this.searching = true; })
        }
      }.width('100%').height(32).padding({ left: 4, right: 4 })
      // 普通 DTO 的同 ID 对象会整体替换；行键包含版本，避免 ForEach 复用旧闭包。
      Stack({ alignContent: Alignment.Top }) {
        List({ space: 2 }) {
          ForEach(this.visibleSessions(), (session: SessionDto) => {
            ListItem() {
              Row({ space: 10 }) {
                Circle().width(7).height(7).fill(this.sessionState(session) === '执行中' || this.sessionState(session) === '待处理' ? Theme.accent : Theme.faint)
                  .margin({ top: 6 })
                Column({ space: 2 }) {
                  Text(session.title).fontSize(14).fontColor(Theme.text).maxLines(1).textOverflow({ overflow: TextOverflow.Ellipsis })
                  Text(`${this.sessionState(session)} · ${session.messages.length} 条消息`).caption()
                }.alignItems(HorizontalAlign.Start).layoutWeight(1)
              }.alignItems(VerticalAlign.Top).width('100%').padding({ left: 10, right: 10, top: 8, bottom: 8 }).borderRadius(9)
                .backgroundColor(this.selected === session.id && this.view === 'chat' ? Theme.selected : Color.Transparent)
            }.onClick(() => { this.select(session.id); })
          }, (session: SessionDto) => `${session.id}:${session.version}`)
        }.width('100%').height('100%').scrollBar(BarState.Off)
        if (this.visibleSessions().length === 0) {
          Text(this.sessionSearch ? '没有匹配的任务' : '还没有任务。点上方「新任务」开始。')
            .caption().width('100%').padding({ left: 10, right: 10, top: 10 }).textAlign(TextAlign.Start)
        }
      }.layoutWeight(1).width('100%').margin({ top: 4 })
      Row({ space: 8 }) {
        Circle().width(8).height(8).fill(this.connected ? Theme.accent : Theme.faint)
        Text(this.connected ? (this.connection.capabilities?.instance_name ?? '已连接') : (this.busy ? '正在连接…' : '未连接'))
          .caption().layoutWeight(1).maxLines(1)
        if (!this.connected && !this.busy) {
          Text('重连').fontSize(12).fontColor(Theme.accent).onClick(() => { this.connect(''); })
        }
      }.width('100%').padding({ left: 6, top: 8 })
    }.height('100%').width('100%').padding({ left: 16, right: 16, top: 12 + this.top(), bottom: 12 + this.bottom() }).backgroundColor(Theme.sidebar)
      // 在侧栏内向左滑即可收起，与右拉唤出对称。
      .onTouch((event: TouchEvent) => { this.swipeClose(event); })
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
  // 工具输入与输出默认折叠，点击调用行展开；与过程折叠共用 processExpanded，键带序号区分。
  private toolKey(row: ConversationRow, entry: ProcessEntry): string { return `${row.key}#${entry.sequence}`; }
  private isToolExpanded(row: ConversationRow, entry: ProcessEntry): boolean {
    return this.processExpanded[this.toolKey(row, entry)] ?? false;
  }
  private hasToolDetail(observation: ObservationDto | undefined): boolean {
    return !!observation && !!(observation.input_summary || observation.output || observation.truncated || observation.late_result);
  }
  private toggleTool(row: ConversationRow, entry: ProcessEntry): void {
    const next: Record<string, boolean> = {};
    for (const key of Object.keys(this.processExpanded)) { next[key] = this.processExpanded[key]; }
    next[this.toolKey(row, entry)] = !this.isToolExpanded(row, entry);
    this.processExpanded = next;
  }
  @Builder private process(row: ConversationRow) {
    Column({ space: 10 }) {
      Row({ space: 6 }) {
        SymbolGlyph(this.isProcessExpanded(row) ? $r('sys.symbol.chevron_down') : $r('sys.symbol.chevron_right'))
          .fontSize(12).fontColor([Theme.secondary])
        Text(row.complete ? '执行过程' : '正在执行').fontSize(13).fontColor(Theme.secondary).fontWeight(FontWeight.Medium)
        Text(`${row.entries?.length ?? 0}`).fontSize(12).fontColor(Theme.secondary)
          .padding({ left: 6, right: 6 }).borderRadius(8).backgroundColor(Theme.selected)
      }.padding({ left: 10, right: 10, top: 5, bottom: 5 }).borderRadius(14)
      .backgroundColor(Theme.sidebar).border({ width: 1, color: Theme.line })
      .onClick(() => {
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
              Row({ space: 4 }) {
                Text(`${entry.observation.agent_name} · ${entry.observation.tool_name} · ${entry.observation.kind === 'tool_started' ? '调用' : observationOutcome(entry.observation, '完成', '失败')}`)
                  .fontSize(13).fontColor(Theme.secondary).flexShrink(1)
                if (this.hasToolDetail(entry.observation)) {
                  SymbolGlyph(this.isToolExpanded(row, entry) ? $r('sys.symbol.chevron_down') : $r('sys.symbol.chevron_right'))
                    .fontSize(11).fontColor([Theme.faint])
                }
              }.onClick(() => { if (this.hasToolDetail(entry.observation)) { this.toggleTool(row, entry); } })
              if (this.isToolExpanded(row, entry)) {
                if (entry.observation.input_summary) { Text(entry.observation.input_summary).fontSize(12).fontColor(Theme.secondary).copyOption(CopyOptions.LocalDevice) }
                if (entry.observation.output) { Text(entry.observation.output).fontSize(12).fontColor(Theme.secondary).fontFamily('monospace').copyOption(CopyOptions.LocalDevice) }
                if (entry.observation.truncated) { Text('输出已截断').caption() }
                if (entry.observation.late_result) { Text('任务结束后的返回').caption() }
              }
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
          Flex({ wrap: FlexWrap.Wrap }) {
            ForEach(request.options, (option: ApprovalOptionDto, index: number) => {
              Button(option.label).ghost()
                .fontColor(index === 0 ? Color.White : Theme.text).backgroundColor(index === 0 ? Theme.accent : Color.Transparent)
                .border({ width: index === 0 ? 0 : 1, color: Theme.line, radius: 16 }).margin({ right: 8, bottom: 8 })
                .enabled(this.connected && !this.busy && !this.hasPending && this.connection.supports('approvals.respond'))
                .onClick(() => { this.approve(request, option.id); })
            })
          }
          if (this.hasDenyOption(request)) {
            TextInput({ placeholder: '拒绝理由（可选，会转达给 Agent）', text: this.feedback(request) }).height(36).fontSize(13).backgroundColor(Theme.page)
              .onChange((value: string) => { this.editFeedback(request, value); })
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
  @Builder private summaryEntry() {
    if (this.canManageExperience() && (this.current()?.messages.length ?? 0) > 0 && !this.draftGenerating()) {
      if (this.summaryOpen) {
        Column({ space: 10 }) {
          Text('总结这次的尝试经验').fontSize(15).fontColor(Theme.text).fontWeight(FontWeight.Medium)
          Text('经验管理员会根据本任务的对话和工具记录，整理出试过的路线、失败原因和最终可行的做法，保存前你可以修改。').caption()
          TextInput({ placeholder: '想重点记下什么（可选）', text: this.summaryNote }).height(38).fontSize(14).backgroundColor(Theme.page)
            .onChange((value: string) => { this.summaryNote = value; })
          Row({ space: 10 }) {
            Button('开始总结').primary().height(36).enabled(this.connected && !this.busy).onClick(() => { this.startSummary(this.summaryNote); })
            Button('取消').ghost().height(36).onClick(() => { this.summaryOpen = false; })
          }
        }.card()
      } else {
        Row() {
          Button() {
            Row({ space: 6 }) {
              SymbolGlyph($r('sys.symbol.book')).fontSize(14).fontColor([Theme.text])
              Text('总结经验').fontSize(13).fontColor(Theme.text)
            }
          }.ghost().enabled(this.connected && !this.busy).onClick(() => { this.summaryOpen = true; })
        }.width('100%').justifyContent(FlexAlign.End)
      }
    }
  }
  @Builder private experienceDrafts() {
    ForEach(this.drafts(), (draft: ExperienceDraftDto) => {
      Column({ space: 10 }) {
        Row({ space: 8 }) {
          SymbolGlyph($r('sys.symbol.book')).fontSize(15).fontColor([Theme.accent])
          Text(draft.status === 'generating' ? '正在整理经验…' : draft.status === 'failed' ? '经验总结失败' : '经验草稿')
            .fontSize(15).fontColor(Theme.text).fontWeight(FontWeight.Medium).layoutWeight(1)
        }.width('100%')
        if (draft.note) { Text(`重点：${draft.note}`).caption() }
        if (draft.status === 'generating') {
          Row({ space: 10 }) {
            LoadingProgress().width(20).height(20).color(Theme.accent)
            Text('经验管理员正在阅读本任务的对话与工具记录').caption().layoutWeight(1)
            Text('取消').fontSize(13).fontColor(Theme.secondary).enabled(!this.busy).onClick(() => { this.discardDraft(draft, false); })
          }.width('100%')
        } else if (draft.status === 'failed') {
          Text(draft.error ?? '未知错误').fontSize(13).fontColor(Theme.danger).copyOption(CopyOptions.LocalDevice)
          Row({ space: 10 }) {
            Button('重试').ghost().enabled(this.connected && !this.busy).onClick(() => { this.discardDraft(draft, true); })
            Button('关闭').ghost().enabled(!this.busy).onClick(() => { this.discardDraft(draft, false); })
          }
        } else {
          if (draft.merge_target) {
            Row({ space: 8 }) {
              Text(this.draftAsNew[draft.draft_id] ? `另存为新经验（与「${draft.merge_target.title}」相似）` : `将合并更新已有经验「${draft.merge_target.title}」`)
                .fontSize(13).fontColor(Theme.warn).layoutWeight(1)
              Text(this.draftAsNew[draft.draft_id] ? '改为合并' : '另存为新经验').fontSize(13).fontColor(Theme.accent)
                .onClick(() => { this.draftAsNew = this.copyRecord(this.draftAsNew, draft.draft_id, !(this.draftAsNew[draft.draft_id] ?? false)); })
            }.width('100%')
          }
          TextInput({ placeholder: '标题', text: this.draftTitle(draft) }).height(40).fontSize(15).backgroundColor(Theme.page)
            .onChange((value: string) => { this.draftTitles = this.copyRecord(this.draftTitles, draft.draft_id, value); })
          if (this.draftEditing[draft.draft_id]) {
            TextArea({ text: this.draftBody(draft) }).height(280).fontSize(14).fontFamily('monospace').backgroundColor(Theme.page)
              .onChange((value: string) => { this.draftBodies = this.copyRecord(this.draftBodies, draft.draft_id, value); })
          } else {
            Column() { MarkdownView({ source: this.draftBody(draft), fontSize: 14 }) }
              .width('100%').padding(12).backgroundColor(Theme.page).borderRadius(10)
          }
          Row({ space: 10 }) {
            Button(this.mergeTarget(draft) ? '合并保存' : '保存到经验库').primary().height(36)
              .enabled(this.connected && !this.busy && this.draftTitle(draft).trim().length > 0 && this.draftBody(draft).trim().length > 0)
              .onClick(() => { this.saveDraft(draft); })
            Button(this.draftEditing[draft.draft_id] ? '预览' : '编辑正文').ghost().height(36)
              .onClick(() => { this.draftEditing = this.copyRecord(this.draftEditing, draft.draft_id, !(this.draftEditing[draft.draft_id] ?? false)); })
            Blank()
            Text('丢弃').fontSize(13).fontColor(Theme.secondary).enabled(!this.busy).onClick(() => { this.discardDraft(draft, false); })
          }.width('100%')
        }
      }.card().backgroundColor(draft.status === 'failed' ? Theme.warnSoft : Theme.surface)
    }, (draft: ExperienceDraftDto) => `${draft.draft_id}:${draft.status}`)
  }
  @Builder private experiencesBody() {
    if (this.experienceDetail) {
      Column({ space: 12 }) {
        Text('‹ 返回列表').fontSize(14).fontColor(Theme.accent).onClick(() => { this.experienceDetail = undefined; this.experienceEditing = false; })
        if (this.experienceEditing) {
          TextInput({ placeholder: '标题', text: this.experienceTitle }).height(40).backgroundColor(Theme.page)
            .onChange((value: string) => { this.experienceTitle = value; })
          TextArea({ text: this.experienceBody }).height(360).fontSize(14).fontFamily('monospace').backgroundColor(Theme.page)
            .onChange((value: string) => { this.experienceBody = value; })
          Row({ space: 10 }) {
            Button('保存').primary().height(36)
              .enabled(this.connected && !this.busy && this.experienceTitle.trim().length > 0 && this.experienceBody.trim().length > 0)
              .onClick(() => { this.saveExperience(); })
            Button('取消').ghost().height(36).onClick(() => { this.experienceEditing = false; })
          }
        } else {
          Text(this.experienceDetail.title).heading(24).width('100%')
          Text(`更新于 ${this.experienceDetail.updated_at.substring(0, 10)}`).caption()
          MarkdownView({ source: this.experienceDetail.body })
          Row({ space: 16 }) {
            Text('编辑').fontSize(13).fontColor(Theme.accent).enabled(!this.busy).onClick(() => { this.editExperience(); })
            if (this.experienceDeleting === this.experienceDetail.id) {
              Text('确认删除').fontSize(13).fontColor(Theme.danger).enabled(this.connected && !this.busy)
                .onClick(() => { if (this.experienceDetail) { this.deleteExperience(this.experienceDetail.id); } })
              Text('取消').fontSize(13).fontColor(Theme.secondary).onClick(() => { this.experienceDeleting = ''; })
            } else {
              Text('删除').fontSize(13).fontColor(Theme.danger).onClick(() => { this.experienceDeleting = this.experienceDetail?.id ?? ''; })
            }
          }.margin({ top: 8 })
        }
      }.card()
    } else {
      Row({ space: 8 }) {
        TextInput({ placeholder: '搜索标题或适用场景', text: this.experienceQuery }).height(36).fontSize(13).layoutWeight(1).backgroundColor(Theme.surface)
          .onChange((value: string) => { this.experienceQuery = value; })
        Button('刷新').ghost().enabled(this.connected && !this.busy).onClick(() => { this.loadExperiences(); })
      }.width('100%')
      ForEach(this.filteredExperiences(), (item: ExperienceSummaryDto) => {
        Column({ space: 4 }) {
          Text(item.title).fontSize(15).fontColor(Theme.text).fontWeight(FontWeight.Medium)
          Text(item.scenario).fontSize(13).fontColor('#4A4840').maxLines(2).textOverflow({ overflow: TextOverflow.Ellipsis })
          Text(`更新于 ${item.updated_at.substring(0, 10)}`).caption()
        }.width('100%').alignItems(HorizontalAlign.Start).padding({ top: 12, bottom: 12 }).border({ width: { bottom: 1 }, color: Theme.line })
          .onClick(() => { this.openExperience(item.id); })
      }, (item: ExperienceSummaryDto) => `${item.id}:${item.updated_at}`)
      if (this.experiences.length === 0) { Text('还没有经验。在任务里点「总结经验」，确认后的经验会出现在这里。').caption() }
      else if (this.filteredExperiences().length === 0) { Text('没有匹配的经验').caption() }
    }
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
            this.experienceDrafts()
            this.taskControls()
            this.summaryEntry()
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
          Circle().width(6).height(6).fill(item.kind === 'tool_started' || item.background === 'started' ? Theme.faint : item.kind === 'permission_audit' ? Theme.warn :
            item.success ? Theme.accent : Theme.danger)
          Text(`${item.tool_name}`).fontSize(13).fontColor(Theme.text).fontWeight(FontWeight.Medium)
          Text(item.agent_name).caption()
        }
        Text(item.kind === 'permission_audit' ? `权限：${item.action ?? ''} ${item.context ?? ''}` : item.kind === 'tool_started' ? '开始调用' :
          observationOutcome(item, '返回成功', '返回错误')).caption()
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
          Text(this.view === 'automation' ? '自动化' : this.view === 'inbox' ? '通知' : this.view === 'experiences' ? '经验库' : '设备与连接')
            .heading(32).width('100%')
          if (this.view === 'experiences') {
            Text('总结过的试错经验。Agent 遇到类似问题时会先查这里，避开已验证走不通的路线。').fontSize(14).fontColor(Theme.secondary).width('100%')
          }
          if (this.view === 'automation') { Text('定时运行的任务，结果回到所选任务中。').fontSize(14).fontColor(Theme.secondary).width('100%') }
          if (this.view === 'inbox') {
            Text(this.connection.capabilities?.meow_enabled ? '重要事件由服务端经 MeoW 推送。' : '未配置 MeoW 推送，待办仍会在任务中显示。')
              .fontSize(14).fontColor(Theme.secondary).width('100%')
          }
          if (this.status) { Text(this.status).caption().width('100%') }
          if (this.view === 'automation') { this.automationBody() }
          else if (this.view === 'inbox') { this.inboxBody() }
          else if (this.view === 'experiences') { this.experiencesBody() }
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
        Text(`${plan.schedule}${plan.next_trigger ? ` · 下次 ${formatLocalTime(plan.next_trigger)}` : ''}`).caption().copyOption(CopyOptions.LocalDevice)
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
        // 从左侧向右拉打开侧栏。用 touch 而非 PanGesture：PanGesture 的
        // onActionStart 里 fingerList 可能为空，取不到真实起手点。
        // 起手范围见下方判断：左侧 1/3 屏宽，贴边几像素会被系统侧边应用栏抢走。
        .onTouch((event: TouchEvent) => {
          if (event.type === TouchType.Down) {
            this.panStartX = event.touches.length > 0 ? event.touches[0].x : -1;
            this.panStartY = event.touches.length > 0 ? event.touches[0].y : -1;
            return;
          }
          if (event.type !== TouchType.Move || event.touches.length === 0) { return; }
          // 侧栏已开时不再处理；宽屏下侧栏可嵌入显示，关掉后同样能用右滑唤回。
          // 起手范围取屏幕左侧 1/3：贴边几像素会被系统侧边应用栏抢走，留足内侧余量。
          if (this.sidebarOpen || this.panStartX < 0 || this.panStartX > this.windowWidth / 3) { return; }
          const dx = event.touches[0].x - this.panStartX;
          const dy = event.touches[0].y - this.panStartY;
          // 水平位移够大且明显偏横向，才认作右拉，避开列表纵向滚动与文本选择。
          if (dx >= 48 && Math.abs(dx) >= Math.abs(dy) * 1.5) {
            this.panStartX = -1;
            this.setSidebar(true);
          }
        })
      if (this.panelOpen && this.view === 'chat' && this.current() && !this.wide()) { Column() { this.panel() }.width('100%').height('100%')
        .transition(TransitionEffect.move(TransitionEdge.END)) }
      if (this.sidebarOpen && !this.wide()) {
        Column().width('100%').height('100%').backgroundColor('#33000000').onClick(() => { this.setSidebar(false); })
          // 遮罩上左滑同样收起，和侧栏内的手势保持一致。
          .onTouch((event: TouchEvent) => { this.swipeClose(event); })
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
