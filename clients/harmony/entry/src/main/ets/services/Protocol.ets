export interface MessageDto { role: string; content: string; task_id?: string; }
export interface TaskDto { task_id: string; name: string; status: string; result?: string; agent_name?: string; }
export interface ApprovalOptionDto { id: string; label: string; description: string; }
export interface ApprovalDto {
  request_id: string; agent_name: string; tool_name: string; version: number;
  claimed: boolean; options: ApprovalOptionDto[];
  input_json?: string; context?: string;
}
export interface SessionDto {
  id: string; title: string; version: number; messages: MessageDto[];
  tasks: Record<string, TaskDto>; approvals: Record<string, ApprovalDto>; history_truncated: boolean;
}
export interface SnapshotDto { instance_id: string; cursor: number; sessions: SessionDto[]; }
export interface CapabilitiesDto { protocol_version: number; instance_id: string; instance_name: string; operations: string[]; }
export interface PairDto { instance_id: string; token: string; }
export interface ReceiptDto { command_id: string; session_id: string; status: string; }
export interface CommandDto {
  operation: string; command_id: string; title?: string; session_id?: string; content?: string;
  request_id?: string; expected_version?: number; option_id?: string; feedback?: string;
}
export interface EventBatchDto { cursor?: number; error?: string; }
