import { UIAbility } from '@kit.AbilityKit';
import { hilog } from '@kit.PerformanceAnalysisKit';
import { KeyboardAvoidMode, window } from '@kit.ArkUI';

export default class EntryAbility extends UIAbility {
  onWindowStageCreate(stage: window.WindowStage): void {
    try {
      stage.loadContent('pages/Index', (error) => {
        if (error.code) { hilog.error(0, 'Harness', '无法加载工作台'); return; }
        this.immersive(stage);
      });
    } catch (_) { hilog.error(0, 'Harness', '无法加载工作台'); }
  }

  // 沉浸式：内容铺满状态栏与导航条区域，页面按避让高度（px）自行留白；键盘弹出时压缩页面而非整体上推。
  private immersive(stage: window.WindowStage): void {
    try {
      const main = stage.getMainWindowSync();
      main.setWindowLayoutFullScreen(true).catch(() => {});
      main.getUIContext().setKeyboardAvoidMode(KeyboardAvoidMode.RESIZE);
      const update = (type: window.AvoidAreaType, area: window.AvoidArea) => {
        if (type === window.AvoidAreaType.TYPE_SYSTEM) { AppStorage.setOrCreate('safeTop', area.topRect.height); }
        if (type === window.AvoidAreaType.TYPE_NAVIGATION_INDICATOR) { AppStorage.setOrCreate('safeBottom', area.bottomRect.height); }
      };
      update(window.AvoidAreaType.TYPE_SYSTEM, main.getWindowAvoidArea(window.AvoidAreaType.TYPE_SYSTEM));
      update(window.AvoidAreaType.TYPE_NAVIGATION_INDICATOR, main.getWindowAvoidArea(window.AvoidAreaType.TYPE_NAVIGATION_INDICATOR));
      main.on('avoidAreaChange', (data: window.AvoidAreaOptions) => { update(data.type, data.area); });
    } catch (_) { hilog.error(0, 'Harness', '无法启用沉浸式布局'); }
  }

  onForeground(): void { AppStorage.setOrCreate('harnessForeground', true); }
  onBackground(): void { AppStorage.setOrCreate('harnessForeground', false); }
}
