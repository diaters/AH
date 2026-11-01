export interface MessageDto { role: string; content: string; task_id?: string; phase?: string; sequence?: number; attachment_ids?: string[]; }
export interface TaskDto {
  task_id: string; name: string; status: string; result?: string; agent_name?: string;
  parent_id?: string; revision?: string; can_stop?: boolean; can_finish?: boolean;
}
export interface QuestionDto { task_id: string; question_id: string; question: string; claimed: boolean; }
export interface ApprovalOptionDto { id: string; label: string; description: string; }
// 拒绝类选项：deny 与各通道统一的 reject_with_feedback 都表示拒绝。
// 只有拒绝会携带理由——运行时只在拒绝分支读取 feedback，允许时填写无效果。
export function isDenyOption(optionId: string): boolean {
  return optionId === 'deny' || optionId === 'reject_with_feedback';
}
export interface ApprovalDto {
  request_id: string; agent_name: string; tool_name: string; version: number;
  claimed: boolean; options: ApprovalOptionDto[];
  input_json?: string; context?: string;
}
export interface SessionDto {
  id: string; title: string; version: number; messages: MessageDto[];
  tasks: Record<string, TaskDto>; approvals: Record<string, ApprovalDto>; history_truncated: boolean;
  questions: Record<string, QuestionDto>;
  observations: ObservationDto[]; observations_truncated: boolean; progress: Record<string, ProgressDto>;
  updated_at?: number; experience_drafts?: Record<string, ExperienceDraftDto>;
}
// 经验草稿：generating → ready / failed；保存或丢弃后从会话移除。
export interface ExperienceRefDto { id: string; title: string; }
export interface ExperienceDraftDto {
  draft_id: string; status: string; note?: string; title?: string; body?: string;
  merge_target?: ExperienceRefDto | null; error?: string | null;
}
export interface ExperienceSummaryDto { id: string; title: string; scenario: string; updated_at: string; }
export interface ExperienceListDto { experiences: ExperienceSummaryDto[]; }
export interface ExperienceDto { id: string; title: string; body: string; created_at: string; updated_at: string; source_session?: string; }
export interface ScheduleInputDto { name: string; content: string; schedule: string; enabled: boolean; notify: string; }
export interface ScheduleHealthDto { consecutive_failures: number; alerting: boolean; last_fired_at?: string; }
export interface ScheduleDto extends ScheduleInputDto {
  id: string; revision: string; session_id: string; next_trigger?: string; health: ScheduleHealthDto; archived?: boolean;
}
export interface RunDto { outcome: string; fired_at: string; finished_at: string; result_summary: string; error?: string; }
export interface ScheduleHistoryDto { schedule_id: string; records: RunDto[]; }
export interface AutomationDto {
  available: boolean; schedules: ScheduleDto[]; archived?: ScheduleDto[]; timezone: string;
  stale?: boolean; archive_truncated?: boolean; history?: ScheduleHistoryDto;
}
export interface ScheduleActionDto { kind: string; input?: ScheduleInputDto; schedule_id?: string; expected_revision?: string; }
export interface NotificationDto {
  id: string; session_id: string; title: string; message: string; state: string; result_code?: string;
  created_at: number; next_attempt_at: number; received_at?: number; read_at?: number;
}
export interface ClientFileDto { id: string; session_id: string; name: string; size: number; sha256: string; kind: string; source: string; }
export interface PendingUploadDto { instance_id: string; id: string; session_id: string; name: string; local_path: string; }
export interface ObservationDto { sequence?: number; kind: string; task_id: string; tool_name: string; agent_name: string; action?: string; source?: string; context?: string; input_summary?: string; output?: string; success?: boolean; truncated?: boolean; late_result?: boolean; background?: string; }
export interface SnapshotDto {
  instance_id: string; cursor: number; sessions: SessionDto[]; automation: AutomationDto;
  notifications: NotificationDto[]; notifications_truncated: boolean; notification_overflow: number;
  files: ClientFileDto[];
}
export interface CapabilitiesDto { protocol_version: number; instance_id: string; instance_name: string; operations: string[]; meow_enabled?: boolean; }
export interface ApprovalModeDto { mode: string; }
export interface PairDto { instance_id: string; token: string; }
export interface ReceiptDto { command_id: string; session_id: string; status: string; result_code?: string; }
export interface CommandDto {
  operation: string; command_id: string; title?: string; session_id?: string; content?: string;
  request_id?: string; expected_version?: number; option_id?: string; feedback?: string;
  task_id?: string; expected_revision?: string; question_id?: string; answer?: string;
  action?: ScheduleActionDto;
  attachment_ids?: string[];
}
export interface EventBatchDto { cursor?: number; error?: string; }

export interface DeviceDto { id: string; name: string; expires_at: number; revoked: boolean; }
export interface DevicesDto { devices: DeviceDto[]; }
export interface DiagnosticsDto {
  instance_id: string; version: string; storage_available: boolean; cursor: number; event_retention: number;
  sessions: number; pending_approvals: number; pending_questions: number; unknown_commands: number; files: number; notification_overflow: number;
}
export interface TodoDto {content: string; status: string;}
export interface ProgressDto {task_id?: string; batch_id?: string; items?: TodoDto[]; pending?: number; completed?: number; total?: number;}

// 工具结果的阶段文案：转后台的合成结果与后台调用的真实结果分开标注。
export function observationOutcome(observation: ObservationDto, done: string, failed: string): string {
  if (observation.background === 'started') { return '转入后台'; }
  if (observation.background === 'stopped') { return '后台已停止'; }
  if (observation.background === 'finished') { return observation.success ? '后台完成' : '后台失败'; }
  return observation.success ? done : failed;
}

// 服务端下发 UTC（RFC 3339），按设备本地时区显示；无法解析时原样返回。
export function formatLocalTime(value: string): string {
  const date = new Date(value);
  if (isNaN(date.getTime())) { return value; }
  const two = (n: number): string => `${n}`.padStart(2, '0');
  return `${date.getFullYear()}-${two(date.getMonth() + 1)}-${two(date.getDate())} ${two(date.getHours())}:${two(date.getMinutes())}`;
}

// 审批方式：运行时对「需确认的工具调用」的处理。取值与服务端 HARNESS_APPROVAL_MODE 一致。
export interface ApprovalModeChoice { id: string; label: string; description: string; }
export const APPROVAL_MODES: ApprovalModeChoice[] = [
  { id: 'supervisor', label: 'Agent 审批', description: 'watchdog 先判断：安全则批准，越权则拒绝，拿不准才问你' },
  { id: 'manual', label: '手动确认', description: '每次需确认的工具调用都由你决定' },
  { id: 'allow_all', label: '全部同意', description: '需确认的工具直接放行，不再询问（风险最高）' }
];
