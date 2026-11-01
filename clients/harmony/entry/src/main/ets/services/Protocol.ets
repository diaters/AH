export interface MessageDto { role: string; content: string; task_id?: string; phase?: string; sequence?: number; attachment_ids?: string[]; }
export interface TaskDto {
  task_id: string; name: string; status: string; result?: string; agent_name?: string;
  parent_id?: string; revision?: string; can_stop?: boolean; can_finish?: boolean;
}
export interface QuestionDto { task_id: string; question_id: string; question: string; claimed: boolean; }
export interface ApprovalOptionDto { id: string; label: string; description: string; }
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
