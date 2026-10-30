import { common } from '@kit.AbilityKit';
import { preferences } from '@kit.ArkData';
import { fileIo } from '@kit.CoreFileKit';
import { util } from '@kit.ArkTS';
import { Connection, CredentialStore } from '../services/Connection';
import { ApprovalDto, ApprovalOptionDto, CommandDto, MessageDto, SessionDto, TaskDto } from '../services/Protocol';

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
  @StorageLink('harnessForeground') @Watch('lifecycle') foreground: boolean = true;
  private connection: Connection = new Connection();
  private storage?: preferences.Preferences;
  private pending?: CommandDto;
  private refreshing: boolean = false;
  private refreshAgain: boolean = false;

  aboutToAppear(): void {
    try {
    const context = this.getUIContext().getHostContext() as common.UIAbilityContext;
    this.storage = preferences.getPreferencesSync(context, { name: 'harness' });
    this.address = this.storage.getSync('address', '') as string;
    this.instance = this.storage.getSync('instance', '') as string;
    this.caPem = this.storage.getSync('ca', '') as string;
    this.selected = this.storage.getSync('selected', '') as string;
    this.draft = this.storage.getSync(`draft:${this.selected}`, '') as string;
    const saved = this.storage.getSync('pending', '') as string;
    if (saved.length > 0) {
      try { this.pending = JSON.parse(saved) as CommandDto; this.hasPending = true; } catch (_) { this.status = '待发送记录损坏，请检查'; }
    }
    } catch (_) { this.status = '无法读取本地状态，请检查应用存储'; }
  }
  aboutToDisappear(): void { this.connection.close(); }
  private lifecycle(): void {
    if (!this.foreground) { this.connection.close(); this.connected = false; }
    else if (!this.settings && this.instance.length > 0) { this.connect(false); }
  }
  private save(key: string, value: string): boolean {
    try {
      if (!this.storage) { throw new Error('storage unavailable'); }
      this.storage.putSync(key, value); this.storage.flushSync(); return true;
    } catch (_) { this.status = '无法保存本地状态，尚未发送新命令'; return false; }
  }
  private select(id: string): void {
    this.selected = id; this.save('selected', id);
    try { this.draft = this.storage?.getSync(`draft:${id}`, '') as string ?? ''; }
    catch (_) { this.status = '无法读取草稿'; this.draft = ''; }
  }
  private current(): SessionDto | undefined { return this.sessions.find((session: SessionDto) => session.id === this.selected); }
  private tasks(): TaskDto[] { const session = this.current(); return session ? Object.values(session.tasks) : []; }
  private approvals(): ApprovalDto[] { const session = this.current(); return session ? Object.values(session.approvals) : []; }

  private async connect(pair: boolean): Promise<void> {
    if (this.busy) { return; }
    this.busy = true;
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
      this.connected = true; this.settings = false;
      this.status = `已连接 · ${this.connection.capabilities?.instance_name ?? 'Harness'}`;
      await this.connection.subscribe(snapshot.cursor, () => { this.refresh(); }, (text: string) => {
        this.status = text; this.connected = false;
        if (text === 'snapshot_required') { this.connect(false); }
      });
    } catch (error) {
      this.connection.close(); this.connected = false;
      this.status = (error as Error).message || '连接失败，请检查地址、证书和凭证';
    } finally { this.busy = false; }
  }

  private async refresh(): Promise<void> {
    if (this.refreshing) { this.refreshAgain = true; return; }
    this.refreshing = true;
    try {
      do {
        this.refreshAgain = false;
        this.sessions = (await this.connection.snapshot()).sessions;
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
      this.status = receipt.status === 'queued' ? '输入已入队，尚不代表 Agent 已消费' : `处理状态：${receipt.status}`;
      if (command.operation === 'create_session') { this.select(receipt.session_id); }
      if (command.operation === 'send_text' && receipt.status === 'queued') {
        this.save(`draft:${command.session_id ?? ''}`, '');
        if (command.session_id === this.selected) { this.draft = ''; }
      }
      this.pending = undefined; this.hasPending = false; this.save('pending', '');
      await this.refresh();
    } catch (error) { this.status = `${(error as Error).message}。保留原命令，可查询状态或重试`; }
    finally { this.busy = false; }
  }

  private async queryPending(): Promise<void> {
    if (!this.pending) { return; }
    try { this.status = `服务器命令状态：${(await this.connection.status(this.pending.command_id)).status}`; }
    catch (error) { this.status = (error as Error).message; }
  }
  private send(): void {
    if (!this.draft.trim() || this.hasPending) { return; }
    const command: CommandDto = { operation: 'send_text', command_id: util.generateRandomUUID(), session_id: this.selected, content: this.draft };
    this.submit(command);
  }
  private approve(request: ApprovalDto, option: string): void {
    const command: CommandDto = { operation: 'respond_approval', command_id: util.generateRandomUUID(), session_id: this.selected,
      request_id: request.request_id, expected_version: request.version, option_id: option };
    this.submit(command);
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
        Text('凭证保存在系统安全存储中。当前版本支持会话、消息和审批；自动化、附件和通知尚未接通。').fontSize(13).fontColor('#526070')
      }.padding(24).width('100%').constraintSize({ maxWidth: 620 })
    }.width('100%').layoutWeight(1)
  }

  @Builder private sessionList() {
    Column({ space: 12 }) {
      Row() {
        Text('会话').fontSize(22).fontWeight(FontWeight.Bold).layoutWeight(1)
        Button('新建').enabled(this.connected && !this.busy && !this.hasPending && this.connection.supports('sessions.create')).onClick(() => {
          const command: CommandDto = { operation: 'create_session', command_id: util.generateRandomUUID(), title: '新的工作' };
          this.submit(command);
        })
      }
      List({ space: 8 }) {
        ForEach(this.sessions, (session: SessionDto) => {
          ListItem() {
            Column({ space: 6 }) {
              Text(session.title).fontWeight(FontWeight.Medium)
              Text(`${session.messages.length} 条消息 · ${Object.keys(session.tasks).length} 个任务`).fontSize(12).fontColor('#526070')
            }.width('100%').alignItems(HorizontalAlign.Start).padding(14)
              .backgroundColor(this.selected === session.id ? '#DDF3ED' : '#FFFFFF').borderRadius(12)
          }.onClick(() => { this.select(session.id); })
        }, (session: SessionDto) => session.id)
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
          Text(task.status).fontSize(13).fontColor('#526070')
          if (task.result) { Text(task.result).fontSize(13) }
        }.alignItems(HorizontalAlign.Start).width('100%').padding(12).backgroundColor('#FFFFFF').borderRadius(12)
      }, (task: TaskDto) => task.task_id)
      ForEach(this.approvals(), (request: ApprovalDto) => {
        Column({ space: 8 }) {
          Text(`${request.agent_name} 请求 ${request.tool_name}`).fontWeight(FontWeight.Bold)
          Text(request.input_json ?? '').fontSize(12).copyOption(CopyOptions.InApp)
          Text(request.context ?? '').fontSize(12)
          if (request.claimed) { Text('已有设备提交，等待运行时校验').fontColor('#526070') }
          ForEach(request.options, (option: ApprovalOptionDto) => {
            Button(option.label).enabled(!request.claimed && this.connected && !this.busy && !this.hasPending && this.connection.supports('approvals.respond'))
              .onClick(() => { this.approve(request, option.id); })
          })
        }.padding(12).backgroundColor('#FFF4DA').borderRadius(12).width('100%')
      }, (request: ApprovalDto) => request.request_id)
    }.padding(16).width('100%').alignItems(HorizontalAlign.Start)
  }

  @Builder private conversation() {
    Column({ space: 12 }) {
      Row() {
        if (this.windowWidth < 700) { Button('返回').onClick(() => { this.select(''); }) }
        Text(this.current()?.title ?? '选择会话').fontSize(22).fontWeight(FontWeight.Bold).layoutWeight(1)
      }
      Scroll() {
        Column({ space: 12 }) {
          if (this.current()?.history_truncated) { Text('较早的消息已超出当前保留范围').fontSize(12) }
          ForEach(this.current()?.messages ?? [], (message: MessageDto) => {
            Column({ space: 6 }) {
              Text(message.role === 'agent' ? 'Harness' : message.role === 'user' ? '你' : '系统').fontSize(12).fontColor('#526070')
              Text(message.content).fontSize(16).copyOption(CopyOptions.InApp)
            }.width('100%').alignItems(HorizontalAlign.Start).padding(16)
              .backgroundColor(message.role === 'user' ? '#DDF3ED' : '#FFFFFF').borderRadius(14)
          })
          if (this.windowWidth < 1100) { this.detailView() }
        }.width('100%')
      }.layoutWeight(1).width('100%')
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
          Button('设置').onClick(() => { this.connection.close(); this.connected = false; this.settings = true; })
        }
      }.padding(16).width('100%')
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
      else {
        Row() {
          if (this.windowWidth >= 700 || this.selected.length === 0) { this.sessionList() }
          if (this.windowWidth >= 700 || this.selected.length > 0) { this.conversation() }
          if (this.windowWidth >= 1100) { Scroll() { this.detailView() }.width(320).height('100%') }
        }.layoutWeight(1).width('100%').alignItems(VerticalAlign.Top)
      }
    }.height('100%').width('100%').backgroundColor('#F5F7FA')
      .onAreaChange((_old, area) => { this.windowWidth = Number(area.width); })
  }
}
