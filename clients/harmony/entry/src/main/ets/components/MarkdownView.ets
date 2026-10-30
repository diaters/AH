import { InlineSpan, MdBlock, parseMarkdown } from '../common/Markdown';
import { Theme } from '../common/Theme';

// Agent 回复与文本产物共用的 Markdown 渲染。
@Component
export struct MarkdownView {
  @Prop source: string = '';
  fontSize: number = 15;
  color: string = '#4A4840';

  @Builder richText(spans: InlineSpan[]) {
    Text() {
      ForEach(spans, (span: InlineSpan) => {
        if (span.code) {
          Span(span.text).fontFamily('monospace').fontSize(this.fontSize - 1).fontColor(Theme.text)
            .textBackgroundStyle({ color: Theme.selected, radius: 4 })
        } else {
          Span(span.text).fontWeight(span.bold ? FontWeight.Medium : FontWeight.Normal).fontColor(span.bold ? Theme.text : this.color)
        }
      })
    }.fontSize(this.fontSize).lineHeight(this.fontSize + 11).fontColor(this.color).copyOption(CopyOptions.LocalDevice).width('100%')
  }

  @Builder block(item: MdBlock) {
    if (item.kind === 'h') {
      Text(item.spans.map((span: InlineSpan) => span.text).join('')).fontSize(item.level <= 2 ? this.fontSize + 5 : this.fontSize + 2)
        .fontColor(Theme.text).fontFamily(Theme.serif).fontWeight(FontWeight.Medium).margin({ top: 6 }).width('100%')
    } else if (item.kind === 'li') {
      Row({ space: 8 }) {
        Text(item.text).fontSize(this.fontSize).lineHeight(this.fontSize + 11).fontColor(Theme.secondary).constraintSize({ minWidth: 14 })
        Column() { this.richText(item.spans) }.layoutWeight(1)
      }.width('100%').alignItems(VerticalAlign.Top)
    } else if (item.kind === 'code' || item.kind === 'table') {
      Scroll() {
        Text(item.text).fontSize(13).lineHeight(21).fontFamily('monospace').fontColor('#4A4840').copyOption(CopyOptions.LocalDevice)
      }.scrollable(ScrollDirection.Horizontal).scrollBar(BarState.Off).width('100%').align(Alignment.Start)
        .padding(12).backgroundColor(Theme.surface).border({ width: 1, color: Theme.line, radius: 10 })
    } else if (item.kind === 'hr') {
      Divider().color(Theme.line)
    } else {
      this.richText(item.spans)
    }
  }

  build() {
    Column({ space: 10 }) {
      ForEach(parseMarkdown(this.source), (item: MdBlock) => { this.block(item) })
    }.width('100%').alignItems(HorizontalAlign.Start)
  }
}
