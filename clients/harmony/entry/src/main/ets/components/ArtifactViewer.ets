import { common } from '@kit.AbilityKit';
import { fileIo, fileUri } from '@kit.CoreFileKit';
import { util } from '@kit.ArkTS';
import { image } from '@kit.ImageKit';
import { webview } from '@kit.ArkWeb';
import { Connection } from '../services/Connection';
import { ClientFileDto } from '../services/Protocol';
import { Theme } from '../common/Theme';
import { MarkdownView } from './MarkdownView';

// 产物：原机文件（经授权文件 ID 获取）或 Agent 回复中的链接。
export interface ArtifactRef { key: string; kind: string; title: string; detail: string; file?: ClientFileDto; url?: string; }

const TEXT_LIMIT = 512 * 1024;

function extension(name: string): string {
  const index = name.lastIndexOf('.');
  return index < 0 ? '' : name.substring(index + 1).toLowerCase();
}

// 旧版服务端把非图片一律标为 document，按扩展名补齐预览类型。
export function viewerKind(file: ClientFileDto): string {
  if (file.kind !== 'document') { return file.kind; }
  const ext = extension(file.name);
  if (['mp4', 'm4v', 'mov', 'webm', 'mkv'].includes(ext)) { return 'video'; }
  if (ext === 'pdf') { return 'pdf'; }
  if (ext === 'html' || ext === 'htm') { return 'html'; }
  if (['txt', 'md', 'markdown', 'log', 'csv', 'json', 'toml', 'yaml', 'yml', 'xml', 'rs', 'py', 'js', 'ts', 'ets', 'sh', 'sql'].includes(ext)) { return 'text'; }
  return 'document';
}

export function formatSize(size: number): string {
  if (size < 1024) { return `${size} B`; }
  if (size < 1024 * 1024) { return `${(size / 1024).toFixed(1)} KB`; }
  return `${(size / 1024 / 1024).toFixed(1)} MB`;
}

export function fileArtifact(file: ClientFileDto): ArtifactRef {
  return { key: `file:${file.id}`, kind: viewerKind(file), title: file.name,
    detail: `${formatSize(file.size)} · ${file.source === 'upload' ? '我上传的' : 'Agent 产物'}`, file: file };
}

// 提取回复中的链接；localhost 指向原机，替换为当前连接使用的实例主机。
export function linkArtifacts(texts: string[], host: string): ArtifactRef[] {
  const hostname = host.startsWith('[') ? host.substring(0, host.indexOf(']') + 1) : host.split(':')[0];
  const seen: string[] = [];
  const links: ArtifactRef[] = [];
  for (const text of texts) {
    const found = text.match(/https?:\/\/[^\s<>"'`)\]）】，。、；]+/g) ?? [];
    for (const raw of found) {
      let url = raw.replace(/[.,;:!?*]+$/, '');
      if (hostname) { url = url.replace(/^(https?:\/\/)(localhost|127\.0\.0\.1|0\.0\.0\.0)(?=[:/]|$)/, `$1${hostname}`); }
      if (seen.includes(url)) { continue; }
      seen.push(url);
      links.push({ key: `url:${url}`, kind: 'url', title: url.replace(/^https?:\/\//, ''), detail: '链接', url: url });
    }
  }
  return links;
}

export function artifactIcon(kind: string): Resource {
  if (kind === 'image') { return $r('sys.symbol.picture'); }
  if (kind === 'video') { return $r('sys.symbol.play'); }
  if (kind === 'url' || kind === 'html') { return $r('sys.symbol.link'); }
  return $r('sys.symbol.doc_text');
}

async function decodeImage(bytes: ArrayBuffer, max: number): Promise<image.PixelMap> {
  const source = image.createImageSource(bytes);
  try {
    const info = await source.getImageInfo();
    const scale = Math.min(1, max / Math.max(info.size.width, info.size.height));
    return await source.createPixelMap({ desiredSize: { width: Math.max(1, Math.floor(info.size.width * scale)),
      height: Math.max(1, Math.floor(info.size.height * scale)) } });
  } finally { await source.release(); }
}

// 视频、PDF、HTML 需要本地文件路径；同 ID 且长度一致时复用缓存。
async function cachedFile(connection: Connection, file: ClientFileDto, dir: string): Promise<string> {
  const ext = extension(file.name) || 'bin';
  const path = `${dir}/artifact-${file.id}.${ext}`;
  if (fileIo.accessSync(path) && fileIo.statSync(path).size === file.size) { return path; }
  const bytes = await connection.download(file.id);
  if (bytes.byteLength !== file.size) { throw new Error('下载长度不匹配'); }
  const output = fileIo.openSync(path, fileIo.OpenMode.CREATE | fileIo.OpenMode.READ_WRITE | fileIo.OpenMode.TRUNC);
  try { if (fileIo.writeSync(output.fd, bytes) !== bytes.byteLength) { throw new Error('缓存写入不完整'); } }
  finally { fileIo.closeSync(output); }
  return path;
}

// 对话中的图片缩略图，进程内缓存，避免同步刷新时重复下载。
const thumbnails: Map<string, image.PixelMap> = new Map();

@Component
export struct AttachmentThumb {
  file: ClientFileDto = { id: '', session_id: '', name: '', size: 0, sha256: '', kind: '', source: '' };
  connection: Connection = new Connection();
  @State pixel: image.PixelMap | undefined = undefined;
  @State failed: boolean = false;

  aboutToAppear(): void {
    const cached = thumbnails.get(this.file.id);
    if (cached) { this.pixel = cached; return; }
    this.connection.download(this.file.id).then((bytes: ArrayBuffer) => decodeImage(bytes, 480))
      .then((pixel: image.PixelMap) => { thumbnails.set(this.file.id, pixel); this.pixel = pixel; })
      .catch(() => { this.failed = true; });
  }

  build() {
    Stack() {
      if (this.pixel) { Image(this.pixel).width('100%').height('100%').objectFit(ImageFit.Cover) }
      else if (this.failed) { SymbolGlyph($r('sys.symbol.picture')).fontSize(24).fontColor([Theme.faint]) }
      else { LoadingProgress().width(24).height(24).color(Theme.faint) }
    }.width(180).height(128).borderRadius(12).clip(true).backgroundColor(Theme.selected)
  }
}

@Component
export struct ArtifactViewer {
  target: ArtifactRef = { key: '', kind: '', title: '', detail: '' };
  connection: Connection = new Connection();
  onBack: () => void = () => {};
  onSave: (file: ClientFileDto) => void = () => {};
  @State loading: boolean = true;
  @State error: string = '';
  @State pixel: image.PixelMap | undefined = undefined;
  @State text: string = '';
  @State truncated: boolean = false;
  @State local: string = '';
  private web: webview.WebviewController = new webview.WebviewController();
  private video: VideoController = new VideoController();

  aboutToAppear(): void { this.load(); }
  aboutToDisappear(): void { this.pixel?.release().catch(() => {}); this.video.stop(); }

  private async load(): Promise<void> {
    try {
      const context = this.getUIContext().getHostContext() as common.UIAbilityContext;
      const file = this.target.file;
      if (this.target.kind === 'url') { this.local = this.target.url ?? ''; }
      else if (!file) { throw new Error('产物不存在'); }
      else if (this.target.kind === 'image') { this.pixel = await decodeImage(await this.connection.download(file.id), 2048); }
      else if (this.target.kind === 'text') {
        const bytes = await this.connection.download(file.id);
        this.truncated = bytes.byteLength > TEXT_LIMIT;
        this.text = new util.TextDecoder().decodeToString(new Uint8Array(bytes.slice(0, TEXT_LIMIT)));
      } else if (this.target.kind === 'video') {
        this.local = fileUri.getUriFromPath(await cachedFile(this.connection, file, context.cacheDir));
      } else if (this.target.kind === 'pdf' || this.target.kind === 'html') {
        // ArkWeb 读取沙箱文件需要 file:// 加沙箱路径，而非应用 URI。
        this.local = `file://${await cachedFile(this.connection, file, context.cacheDir)}`;
      }
    } catch (error) { this.error = (error as Error).message || '无法获取产物'; }
    finally { this.loading = false; }
  }

  private openExternally(): void {
    const context = this.getUIContext().getHostContext() as common.UIAbilityContext;
    context.openLink(this.target.url ?? '').catch(() => { this.error = '无法用浏览器打开'; });
  }

  private isMarkdown(): boolean { const ext = extension(this.target.title); return ext === 'md' || ext === 'markdown'; }

  build() {
    Column() {
      Row({ space: 6 }) {
        Button() { SymbolGlyph($r('sys.symbol.chevron_left')).fontSize(20).fontColor([Theme.secondary]) }
          .type(ButtonType.Circle).width(36).height(36).backgroundColor(Color.Transparent).onClick(() => { this.onBack(); })
        Column({ space: 2 }) {
          Text(this.target.title).fontSize(14).fontColor(Theme.text).maxLines(1).textOverflow({ overflow: TextOverflow.Ellipsis })
          Text(this.target.detail).fontSize(11).fontColor(Theme.secondary)
        }.alignItems(HorizontalAlign.Start).layoutWeight(1)
        if (this.target.file) {
          Text('保存').fontSize(13).fontColor(Theme.accent).padding(8).onClick(() => { if (this.target.file) { this.onSave(this.target.file); } })
        }
        if (this.target.kind === 'url') {
          Text('浏览器').fontSize(13).fontColor(Theme.accent).padding(8).onClick(() => { this.openExternally(); })
        }
      }.width('100%').padding({ left: 4, right: 8, bottom: 8 })
      Column() {
        if (this.loading) {
          LoadingProgress().width(32).height(32).color(Theme.accent).margin({ top: 48 })
          Text('正在从原机获取…').fontSize(12).fontColor(Theme.secondary)
        } else if (this.error) {
          Text(this.error).fontSize(13).fontColor(Theme.danger).margin({ top: 24 }).padding({ left: 16, right: 16 })
        } else if (this.target.kind === 'image' && this.pixel) {
          Image(this.pixel).width('100%').height('100%').objectFit(ImageFit.Contain)
        } else if (this.target.kind === 'video') {
          Video({ src: this.local, controller: this.video }).width('100%').aspectRatio(16 / 9)
            .controls(true).autoPlay(false).objectFit(ImageFit.Contain).backgroundColor(Color.Black)
        } else if (this.target.kind === 'text') {
          Scroll() {
            Column({ space: 8 }) {
              if (this.isMarkdown()) { MarkdownView({ source: this.text, fontSize: 14 }) }
              else {
                Text(this.text).fontSize(12).lineHeight(19).fontFamily('monospace').fontColor('#4A4840')
                  .copyOption(CopyOptions.LocalDevice).width('100%')
              }
              if (this.truncated) { Text('仅显示前 512 KB，完整内容请保存后查看').fontSize(11).fontColor(Theme.warn) }
            }.padding({ left: 16, right: 16, bottom: 24 })
          }.width('100%').height('100%').align(Alignment.Top).scrollBar(BarState.Auto)
        } else if (this.target.kind === 'url' || this.target.kind === 'pdf' || this.target.kind === 'html') {
          Web({ src: this.local, controller: this.web }).width('100%').height('100%')
            .fileAccess(true).javaScriptAccess(true).domStorageAccess(true)
        } else {
          SymbolGlyph($r('sys.symbol.doc_text')).fontSize(40).fontColor([Theme.faint]).margin({ top: 48 })
          Text('此类型暂不支持预览，可保存到本机后打开').fontSize(13).fontColor(Theme.secondary).margin({ top: 12 })
        }
      }.width('100%').layoutWeight(1).alignItems(HorizontalAlign.Center)
    }.width('100%').height('100%')
  }
}
