import { InlineSpan, MdBlock, MdTable, parseMarkdown, plainText } from '../common/Markdown';
import { Theme } from '../common/Theme';

// Agent 回复与文本产物共用的 Markdown 渲染。
@Component
export struct MarkdownView {
  @Prop source: string = '';
  fontSize: number = 15;
  color: string = '#4A4840';
  // 点击链接时回调；未提供时链接只做样式展示。
  onLink: (url: string) => void = (url: string) => {};

  @Builder richText(spans: InlineSpan[], size: number) {
    Text() {
      ForEach(spans, (span: InlineSpan) => {
        if (span.code) {
          Span(span.text).fontFamily('monospace').fontSize(size - 1).fontColor(Theme.text)
            .textBackgroundStyle({ color: Theme.selected, radius: 4 })
        } else if (span.link.length > 0) {
          Span(span.text).fontColor(Theme.accent).fontWeight(span.bold ? FontWeight.Medium : FontWeight.Normal)
            .fontStyle(span.italic ? FontStyle.Italic : FontStyle.Normal)
            .decoration({ type: TextDecorationType.Underline, color: Theme.accent })
            .onClick(() => { this.onLink(span.link); })
        } else {
          Span(span.text).fontWeight(span.bold ? FontWeight.Medium : FontWeight.Normal).fontColor(span.bold ? Theme.text : this.color)
            .fontStyle(span.italic ? FontStyle.Italic : FontStyle.Normal)
            .decoration({ type: span.strike ? TextDecorationType.LineThrough : TextDecorationType.None, color: this.color })
        }
      })
    }.fontSize(size).lineHeight(size + 11).fontColor(this.color).copyOption(CopyOptions.LocalDevice).width('100%')
  }

  private cellAlign(align: string): TextAlign {
    return align === 'center' ? TextAlign.Center : align === 'end' ? TextAlign.End : TextAlign.Start;
  }

  @Builder tableCell(spans: InlineSpan[], width: number, align: string, header: boolean) {
    Text() {
      ForEach(spans, (span: InlineSpan) => {
        if (span.code) {
          Span(span.text).fontFamily('monospace').fontSize(12).fontColor(Theme.text)
            .textBackgroundStyle({ color: Theme.selected, radius: 4 })
        } else {
          Span(span.text).fontColor(span.link.length > 0 ? Theme.accent : header || span.bold ? Theme.text : this.color)
            .fontWeight(header || span.bold ? FontWeight.Medium : FontWeight.Normal)
            .fontStyle(span.italic ? FontStyle.Italic : FontStyle.Normal)
            .decoration({ type: span.strike ? TextDecorationType.LineThrough : TextDecorationType.None, color: this.color })
            .onClick(() => { if (span.link.length > 0) { this.onLink(span.link); } })
        }
      })
    }.fontSize(13).lineHeight(20).width(width).textAlign(this.cellAlign(align))
      .padding({ left: 10, right: 10, top: 8, bottom: 8 }).copyOption(CopyOptions.LocalDevice)
  }

  // 表格可横向滚动；列宽按内容估算，行之间用细线分隔，表头带底色，数据行隔行着色。
  @Builder table(table: MdTable) {
    Scroll() {
      Column() {
        Row() {
          ForEach(table.header, (cell: InlineSpan[], index: number) => {
            this.tableCell(cell, table.widths[index], table.aligns[index], true)
          })
        }.backgroundColor(Theme.selected).alignItems(VerticalAlign.Top)
        ForEach(table.rows, (row: InlineSpan[][], rowIndex: number) => {
          Row() {
            ForEach(row, (cell: InlineSpan[], index: number) => {
              this.tableCell(cell, table.widths[index], table.aligns[index], false)
            })
          }.backgroundColor(rowIndex % 2 === 1 ? Theme.page : Theme.surface).alignItems(VerticalAlign.Top)
          .border({ width: { top: 1 }, color: Theme.line })
        })
      }.alignItems(HorizontalAlign.Start)
    }.scrollable(ScrollDirection.Horizontal).scrollBar(BarState.Off).width('100%').align(Alignment.Start)
      .border({ width: 1, color: Theme.line, radius: 10 }).clip(true)
  }

  @Builder leaf(item: MdBlock) {
    if (item.kind === 'h') {
      Text(plainText(item.spans)).fontSize(item.level <= 2 ? this.fontSize + 5 : this.fontSize + 2)
        .fontColor(Theme.text).fontFamily(Theme.serif).fontWeight(FontWeight.Medium).margin({ top: 6 }).width('100%')
    } else if (item.kind === 'li') {
      Row({ space: 8 }) {
        if (item.check.length > 0) {
          SymbolGlyph(item.check === 'done' ? $r('sys.symbol.checkmark_square_fill') : $r('sys.symbol.square'))
            .fontSize(this.fontSize).fontColor([item.check === 'done' ? Theme.accent : Theme.secondary]).margin({ top: 5 })
        } else {
          Text(item.text).fontSize(this.fontSize).lineHeight(this.fontSize + 11).fontColor(Theme.secondary).constraintSize({ minWidth: 14 })
        }
        Column() { this.richText(item.spans, this.fontSize) }.layoutWeight(1)
      }.width('100%').alignItems(VerticalAlign.Top).padding({ left: item.indent * 18 })
    } else if (item.kind === 'code') {
      Scroll() {
        Text(item.text).fontSize(13).lineHeight(21).fontFamily('monospace').fontColor('#4A4840').copyOption(CopyOptions.LocalDevice)
      }.scrollable(ScrollDirection.Horizontal).scrollBar(BarState.Off).width('100%').align(Alignment.Start)
        .padding(12).backgroundColor(Theme.surface).border({ width: 1, color: Theme.line, radius: 10 })
    } else if (item.kind === 'table' && item.table) {
      this.table(item.table)
    } else if (item.kind === 'hr') {
      Divider().color(Theme.line)
    } else {
      this.richText(item.spans, this.fontSize)
    }
  }

  @Builder block(item: MdBlock) {
    if (item.kind === 'quote') {
      Column({ space: 8 }) {
        ForEach(item.children ?? [], (child: MdBlock) => { this.leaf(child) })
      }.width('100%').alignItems(HorizontalAlign.Start).padding({ left: 12, top: 2, bottom: 2 })
        .border({ width: { left: 3 }, color: Theme.line })
    } else {
      this.leaf(item)
    }
  }

  build() {
    Column({ space: 10 }) {
      ForEach(parseMarkdown(this.source), (item: MdBlock) => { this.block(item) })
    }.width('100%').alignItems(HorizontalAlign.Start)
  }
}
