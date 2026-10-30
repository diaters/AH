// 极简纸感配色：暖白底、细线分隔、低饱和灰绿强调色。
export class Theme {
  static readonly page: string = '#F4F2EC';
  static readonly sidebar: string = '#EEEBE3';
  static readonly surface: string = '#FBFAF6';
  static readonly selected: string = '#E3E0D6';
  static readonly line: string = '#E0DCD1';
  static readonly text: string = '#26251F';
  static readonly secondary: string = '#8C887D';
  static readonly faint: string = '#B7B2A6';
  static readonly accent: string = '#6F8B73';
  static readonly accentSoft: string = '#E1E8DF';
  static readonly warn: string = '#A7792E';
  static readonly warnSoft: string = '#F4ECDC';
  static readonly danger: string = '#B0533F';
  static readonly serif: string = 'serif';
  // 宽于该值（vp）时侧栏内嵌、可展开右侧面板；更窄时侧栏改为抽屉。
  static readonly wide: number = 840;
  static readonly contentMax: number = 760;
}
