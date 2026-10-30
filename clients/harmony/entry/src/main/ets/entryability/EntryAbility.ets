import { UIAbility } from '@kit.AbilityKit';
import { hilog } from '@kit.PerformanceAnalysisKit';
import { window } from '@kit.ArkUI';

export default class EntryAbility extends UIAbility {
  onWindowStageCreate(stage: window.WindowStage): void {
    try {
      stage.loadContent('pages/Index', (error) => {
        if (error.code) { hilog.error(0, 'Harness', '无法加载工作台'); }
      });
    } catch (_) { hilog.error(0, 'Harness', '无法加载工作台'); }
  }
  onForeground(): void { AppStorage.setOrCreate('harnessForeground', true); }
  onBackground(): void { AppStorage.setOrCreate('harnessForeground', false); }
}
