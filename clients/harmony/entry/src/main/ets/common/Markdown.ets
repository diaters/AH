// 轻量 Markdown：覆盖 Agent 回复中常见的 GFM 子集——标题、段落、有序/无序/任务列表（含缩进）、
// 引用、代码块、表格、分隔线，以及行内粗体、斜体、删除线、行内代码与链接；其余按纯文本显示。
export interface InlineSpan { text: string; bold: boolean; italic: boolean; strike: boolean; code: boolean; link: string; }
export interface MdTable { header: InlineSpan[][]; aligns: string[]; rows: InlineSpan[][][]; widths: number[]; }
// li 的 text 存放序号或圆点，check 为 '' / 'todo' / 'done'；quote 的内容在 children 中（不再嵌套引用）。
export interface MdBlock {
  kind: string; text: string; level: number; spans: InlineSpan[]; indent: number; check: string;
  table?: MdTable; children?: MdBlock[];
}

interface InlineStyle { bold: boolean; italic: boolean; strike: boolean; }

function span(text: string, style: InlineStyle, code: boolean, link: string): InlineSpan {
  return { text: text, bold: style.bold, italic: style.italic, strike: style.strike, code: code, link: link };
}

// 依次匹配：行内代码、链接、裸 URL、粗体、删除线、斜体。下划线强调要求两侧不是字母数字，避免误伤 snake_case。
const INLINE = /(`[^`]+`|\[[^\]]+\]\([^)\s]+\)|https?:\/\/[^\s<>"'`)\]）】，。、；]+|\*\*[^*]+\*\*|__[^_]+__|~~[^~]+~~|\*[^*\s][^*]*\*|(?<![A-Za-z0-9_])_[^_\s][^_]*_(?![A-Za-z0-9_]))/g;

function parseInline(text: string, style: InlineStyle, link: string, out: InlineSpan[]): void {
  const pattern = new RegExp(INLINE.source, 'g');
  let last = 0;
  let match: RegExpExecArray | null = pattern.exec(text);
  while (match !== null) {
    if (match.index > last) { out.push(span(text.substring(last, match.index), style, false, link)); }
    const token = match[0];
    if (token.startsWith('`')) {
      out.push(span(token.substring(1, token.length - 1), style, true, link));
    } else if (token.startsWith('[')) {
      const split = token.indexOf('](');
      parseInline(token.substring(1, split), style, token.substring(split + 2, token.length - 1), out);
    } else if (token.startsWith('http')) {
      const url = token.replace(/[.,;:!?*]+$/, '');
      out.push(span(url, style, false, url));
      if (url.length < token.length) { out.push(span(token.substring(url.length), style, false, link)); }
    } else if (token.startsWith('**') || token.startsWith('__')) {
      parseInline(token.substring(2, token.length - 2), { bold: true, italic: style.italic, strike: style.strike }, link, out);
    } else if (token.startsWith('~~')) {
      parseInline(token.substring(2, token.length - 2), { bold: style.bold, italic: style.italic, strike: true }, link, out);
    } else {
      parseInline(token.substring(1, token.length - 1), { bold: style.bold, italic: true, strike: style.strike }, link, out);
    }
    last = match.index + token.length;
    match = pattern.exec(text);
  }
  if (last < text.length) { out.push(span(text.substring(last), style, false, link)); }
}

export function inline(text: string): InlineSpan[] {
  const spans: InlineSpan[] = [];
  parseInline(text, { bold: false, italic: false, strike: false }, '', spans);
  return spans;
}

export function plainText(spans: InlineSpan[]): string {
  return spans.map((item: InlineSpan) => item.text).join('');
}

function block(kind: string, text: string, level: number): MdBlock {
  return { kind: kind, text: text, level: level, spans: kind === 'code' ? [] : inline(text), indent: 0, check: '' };
}

// 按未转义、不在行内代码里的竖线切分单元格，去掉首尾竖线。
export function splitRow(line: string): string[] {
  let row = line.trim();
  if (row.startsWith('|')) { row = row.substring(1); }
  if (row.endsWith('|') && !row.endsWith('\\|')) { row = row.substring(0, row.length - 1); }
  const cells: string[] = [];
  let current = '';
  let inCode = false;
  for (let i = 0; i < row.length; i++) {
    const ch = row.charAt(i);
    if (ch === '\\' && row.charAt(i + 1) === '|') { current += '|'; i++; continue; }
    if (ch === '`') { inCode = !inCode; }
    if (ch === '|' && !inCode) { cells.push(current.trim()); current = ''; continue; }
    current += ch;
  }
  cells.push(current.trim());
  return cells;
}

function isTableSeparator(line: string): boolean {
  const trimmed = line.trim();
  if (!trimmed.includes('-')) { return false; }
  const cells = splitRow(trimmed);
  return cells.length > 0 && cells.every((cell: string) => /^:?-{1,}:?$/.test(cell));
}

// 显示宽度：中日韩与全角字符按 2 计。
function displayWidth(text: string): number {
  let width = 0;
  for (let i = 0; i < text.length; i++) { width += text.charCodeAt(i) >= 0x2E80 ? 2 : 1; }
  return width;
}

function buildTable(header: string[], separator: string, body: string[]): MdTable {
  const columns = header.length;
  const fit = (cells: string[]): string[] => {
    const fitted = cells.slice(0, columns);
    while (fitted.length < columns) { fitted.push(''); }
    return fitted;
  };
  const aligns = fit(splitRow(separator)).map((cell: string) =>
    cell.startsWith(':') && cell.endsWith(':') ? 'center' : cell.endsWith(':') ? 'end' : 'start');
  const raw = body.map((line: string) => fit(splitRow(line)));
  const widths: number[] = [];
  for (let c = 0; c < columns; c++) {
    let widest = displayWidth(header[c]);
    for (const row of raw) { widest = Math.max(widest, displayWidth(row[c])); }
    // 13 号字约 6.8vp 每半角字符，加上左右内边距；过宽的列换行显示。
    widths.push(Math.min(240, Math.max(56, Math.round(widest * 6.8 + 24))));
  }
  return {
    header: header.map((cell: string) => inline(cell)), aligns: aligns,
    rows: raw.map((row: string[]) => row.map((cell: string) => inline(cell))), widths: widths,
  };
}

export function parseMarkdown(source: string): MdBlock[] {
  const blocks: MdBlock[] = [];
  const lines = source.replace(/\r\n/g, '\n').split('\n');
  let paragraph: string[] = [];
  const flush = () => { if (paragraph.length > 0) { blocks.push(block('p', paragraph.join('\n'), 0)); paragraph = []; } };
  let i = 0;
  while (i < lines.length) {
    const line = lines[i];
    const trimmed = line.trim();
    if (trimmed.startsWith('```') || trimmed.startsWith('~~~')) {
      flush();
      const fence = trimmed.substring(0, 3);
      const code: string[] = [];
      i++;
      while (i < lines.length && !lines[i].trim().startsWith(fence)) { code.push(lines[i]); i++; }
      blocks.push(block('code', code.join('\n'), 0));
    } else if (trimmed.includes('|') && i + 1 < lines.length && isTableSeparator(lines[i + 1])
      && splitRow(lines[i + 1]).length === splitRow(trimmed).length) {
      flush();
      const header = splitRow(trimmed);
      const separator = lines[i + 1];
      const body: string[] = [];
      i += 2;
      while (i < lines.length && lines[i].trim().length > 0 && lines[i].includes('|')) { body.push(lines[i]); i++; }
      const table = block('table', '', 0);
      table.table = buildTable(header, separator, body);
      blocks.push(table);
      continue;
    } else if (trimmed.startsWith('>')) {
      flush();
      const quoted: string[] = [];
      while (i < lines.length && lines[i].trim().startsWith('>')) {
        quoted.push(lines[i].trim().replace(/^(>\s?)+/, ''));
        i++;
      }
      const quote = block('quote', '', 0);
      quote.children = parseMarkdown(quoted.join('\n'));
      blocks.push(quote);
      continue;
    } else if (/^#{1,6}\s/.test(trimmed)) {
      flush();
      const level = trimmed.indexOf(' ');
      blocks.push(block('h', trimmed.substring(level + 1).replace(/\s+#+\s*$/, ''), level));
    } else if (/^(-{3,}|\*{3,}|_{3,})$/.test(trimmed.replace(/\s/g, ''))) {
      flush();
      blocks.push(block('hr', '', 0));
    } else if (/^([-*+]|\d+[.)])\s/.test(trimmed)) {
      flush();
      const ordered = /^\d+/.test(trimmed);
      const marker = trimmed.indexOf(' ');
      let body = trimmed.substring(marker + 1);
      let check = '';
      const task = /^\[([ xX])\]\s/.exec(body);
      if (!ordered && task !== null) { check = task[1] === ' ' ? 'todo' : 'done'; body = body.substring(task[0].length); }
      const item = block('li', body, 0);
      item.text = ordered ? trimmed.substring(0, marker) : '•';
      item.check = check;
      // 两个空格（或一个 Tab）为一级缩进，最多三级。
      const leading = line.length - line.replace(/^[ \t]+/, '').length;
      item.indent = Math.min(3, Math.floor(line.substring(0, leading).replace(/\t/g, '  ').length / 2));
      blocks.push(item);
    } else if (trimmed.length === 0) {
      flush();
    } else {
      paragraph.push(trimmed);
    }
    i++;
  }
  flush();
  return blocks;
}
