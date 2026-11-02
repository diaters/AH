import { UsageBucketDto, UsageDto } from '../services/Protocol';
import { Theme } from '../common/Theme';

@Extend(Text) function caption() {
  .fontSize(12).fontColor(Theme.secondary)
}

interface RangeChoice { id: string; label: string; }
const RANGES: RangeChoice[] = [
  { id: 'today', label: '今天' }, { id: '7d', label: '近 7 天' }, { id: '30d', label: '近 30 天' }, { id: 'month', label: '本月' },
];
const KIND_LABELS: Record<string, string> = {
  'task': '任务执行', 'summarization': '记忆摘要', 'evaluation': '完成度评估', 'experience': '经验库', 'watchdog': '看门狗',
};

export function formatTokens(value: number): string {
  if (value >= 1000000) { return `${(value / 1000000).toFixed(2)}M`; }
  if (value >= 1000) { return `${(value / 1000).toFixed(1)}K`; }
  return `${value}`;
}

export function formatCost(bucket: UsageBucketDto): string {
  if (bucket.calls === 0) { return '¥0'; }
  if (bucket.unpriced_calls === bucket.calls) { return '未配置单价'; }
  const cost = bucket.cost < 0.01 && bucket.cost > 0 ? '<¥0.01' : `¥${bucket.cost.toFixed(2)}`;
  return bucket.unpriced_calls > 0 ? `${cost}+` : cost;
}

// 用量页：总计卡片、每日柱状条、按模型 / Agent / 类型拆分、消耗最多的会话。
@Component
export struct UsageView {
  @Prop usage: UsageDto | undefined = undefined;
  @Prop range: string = '7d';
  @Prop loading: boolean = false;
  onRange: (range: string) => void = (range: string) => {};
  onOpenSession: (sessionId: string) => void = (sessionId: string) => {};

  private tokens(bucket: UsageBucketDto): number { return bucket.input_tokens + bucket.output_tokens; }
  private maxDay(): number {
    let max = 1;
    for (const day of this.usage?.by_day ?? []) { max = Math.max(max, this.tokens(day)); }
    return max;
  }

  @Builder stat(label: string, value: string, detail: string) {
    Column({ space: 4 }) {
      Text(label).fontSize(12).fontColor(Theme.secondary)
      Text(value).fontSize(24).fontColor(Theme.text).fontFamily(Theme.serif).fontWeight(FontWeight.Medium)
      Text(detail).fontSize(12).fontColor(Theme.secondary).maxLines(1).textOverflow({ overflow: TextOverflow.Ellipsis })
    }.alignItems(HorizontalAlign.Start).layoutWeight(1).padding(14)
      .backgroundColor(Theme.surface).border({ width: 1, color: Theme.line, radius: 12 })
  }

  @Builder row(title: string, bucket: UsageBucketDto, onTap?: () => void) {
    Row({ space: 10 }) {
      Column({ space: 2 }) {
        Text(title).fontSize(14).fontColor(Theme.text).maxLines(1).textOverflow({ overflow: TextOverflow.Ellipsis })
        Text(`${bucket.calls} 次 · 输入 ${formatTokens(bucket.input_tokens)} · 输出 ${formatTokens(bucket.output_tokens)}`)
          .fontSize(12).fontColor(Theme.secondary)
      }.alignItems(HorizontalAlign.Start).layoutWeight(1)
      Text(formatCost(bucket)).fontSize(14).fontColor(bucket.unpriced_calls === bucket.calls ? Theme.secondary : Theme.text)
    }.width('100%').padding({ top: 10, bottom: 10 }).border({ width: { bottom: 1 }, color: Theme.line })
    .onClick(() => { if (onTap) { onTap(); } })
  }

  @Builder section(title: string, buckets: UsageBucketDto[], labels: Record<string, string>) {
    if (buckets.length > 0) {
      Text(title).caption().width('100%').margin({ top: 8 })
      ForEach(buckets, (bucket: UsageBucketDto) => { this.row(labels[bucket.key] ?? bucket.key, bucket) },
        (bucket: UsageBucketDto) => `${title}:${bucket.key}`)
    }
  }

  build() {
    Column({ space: 14 }) {
      Row({ space: 8 }) {
        ForEach(RANGES, (choice: RangeChoice) => {
          Text(choice.label).fontSize(13).padding({ left: 12, right: 12, top: 6, bottom: 6 }).borderRadius(14)
            .fontColor(choice.id === this.range ? Color.White : Theme.text)
            .backgroundColor(choice.id === this.range ? Theme.accent : Theme.surface)
            .border({ width: choice.id === this.range ? 0 : 1, color: Theme.line, radius: 14 })
            .onClick(() => { if (!this.loading) { this.onRange(choice.id); } })
        }, (choice: RangeChoice) => choice.id)
      }.width('100%')
      if (this.usage === undefined) {
        Text(this.loading ? '正在读取用量…' : '暂无用量数据').caption().width('100%')
      } else {
        Row({ space: 10 }) {
          this.stat('费用', formatCost(this.usage.total), `${this.usage.from} 至 ${this.usage.to}`)
          this.stat('Token', formatTokens(this.tokens(this.usage.total)),
            `输入 ${formatTokens(this.usage.total.input_tokens)} · 缓存 ${formatTokens(this.usage.total.cached_input_tokens)}`)
        }.width('100%')
        Text(`${this.usage.total.calls} 次模型调用` + (this.usage.total.unpriced_calls > 0 ?
          `，其中 ${this.usage.total.unpriced_calls} 次的模型未配置单价，费用未计入` : '')).caption().width('100%')
        if (this.usage.by_day.length > 1) {
          Text('每日 Token').caption().width('100%').margin({ top: 8 })
          Row({ space: 3 }) {
            ForEach(this.usage.by_day, (day: UsageBucketDto) => {
              Column() {
                Blank()
                Column().width('100%').height(`${Math.max(2, Math.round(this.tokens(day) / this.maxDay() * 100))}%`)
                  .backgroundColor(this.tokens(day) > 0 ? Theme.accent : Theme.line).borderRadius(2)
              }.layoutWeight(1).height('100%')
            }, (day: UsageBucketDto) => day.key)
          }.width('100%').height(96)
          Row() {
            Text(this.usage.by_day[0].key.substring(5)).fontSize(11).fontColor(Theme.secondary).layoutWeight(1)
            Text(this.usage.by_day[this.usage.by_day.length - 1].key.substring(5)).fontSize(11).fontColor(Theme.secondary)
          }.width('100%')
        }
        this.section('按模型', this.usage.by_model, {})
        this.section('按 Agent', this.usage.by_agent, {})
        this.section('按调用类型', this.usage.by_kind, KIND_LABELS)
        if (this.usage.sessions.length > 0) {
          Text('消耗最多的会话').caption().width('100%').margin({ top: 8 })
          ForEach(this.usage.sessions, (session: UsageBucketDto) => {
            this.row(session.title || '未命名任务', session, () => { this.onOpenSession(session.key); })
          }, (session: UsageBucketDto) => `session:${session.key}`)
        }
        if (this.usage.other.calls > 0) {
          this.row('其他来源（IM 通道、定时任务、其他用户）', this.usage.other)
        }
      }
    }.width('100%')
  }
}
