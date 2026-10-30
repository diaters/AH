// 轻量 Markdown：只覆盖 Agent 回复中常见的块级结构与粗体/行内代码，其余按纯文本显示。
export interface InlineSpan { text: string; bold: boolean; code: boolean; }
export interface MdBlock { kind: string; text: string; level: number; spans: InlineSpan[]; }

export function inline(text: string): InlineSpan[] {
  const spans: InlineSpan[] = [];
  const pattern = /(\*\*[^*]+\*\*|`[^`]+`)/g;
  let last = 0;
  let match: RegExpExecArray | null = pattern.exec(text);
  while (match !== null) {
    if (match.index > last) { spans.push({ text: text.substring(last, match.index), bold: false, code: false }); }
    const token = match[0];
    if (token.startsWith('**')) { spans.push({ text: token.substring(2, token.length - 2), bold: true, code: false }); }
    else { spans.push({ text: token.substring(1, token.length - 1), bold: false, code: true }); }
    last = match.index + token.length;
    match = pattern.exec(text);
  }
  if (last < text.length) { spans.push({ text: text.substring(last), bold: false, code: false }); }
  return spans;
}

function block(kind: string, text: string, level: number): MdBlock {
  return { kind: kind, text: text, level: level, spans: kind === 'code' || kind === 'table' ? [] : inline(text) };
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
    if (trimmed.startsWith('```')) {
      flush();
      const code: string[] = [];
      i++;
      while (i < lines.length && !lines[i].trim().startsWith('```')) { code.push(lines[i]); i++; }
      blocks.push(block('code', code.join('\n'), 0));
    } else if (trimmed.startsWith('|')) {
      flush();
      const rows: string[] = [];
      while (i < lines.length && lines[i].trim().startsWith('|')) {
        const row = lines[i].trim();
        // 跳过 |---|---| 分隔行，单元格以两个空格分隔，保持可读而不做列对齐。
        if (!/^\|[\s:|-]+\|$/.test(row)) {
          rows.push(row.substring(1, row.endsWith('|') ? row.length - 1 : row.length).split('|')
            .map((cell: string) => cell.trim().replace(/\*\*/g, '').replace(/`/g, '')).join('  ·  '));
        }
        i++;
      }
      blocks.push(block('table', rows.join('\n'), 0));
      continue;
    } else if (/^#{1,6}\s/.test(trimmed)) {
      flush();
      const level = trimmed.indexOf(' ');
      blocks.push(block('h', trimmed.substring(level + 1), level));
    } else if (/^([-*+]|\d+\.)\s/.test(trimmed)) {
      flush();
      const ordered = /^\d+\./.test(trimmed);
      const marker = trimmed.indexOf(' ');
      // 列表项的 text 存放序号或圆点，正文在 spans 中。
      const item = block('li', trimmed.substring(marker + 1), 0);
      item.text = ordered ? trimmed.substring(0, marker) : '•';
      blocks.push(item);
    } else if (/^(-{3,}|\*{3,})$/.test(trimmed)) {
      flush();
      blocks.push(block('hr', '', 0));
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
