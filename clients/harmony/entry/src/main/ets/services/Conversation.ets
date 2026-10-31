import { MessageDto, ObservationDto, SessionDto } from './Protocol';

export interface ProcessEntry { sequence: number; message?: MessageDto; observation?: ObservationDto; }
export interface ConversationRow { key: string; message?: MessageDto; entries?: ProcessEntry[]; complete?: boolean; taskId?: string; }

// 使用服务端持久化的序号恢复文字与工具事件的顺序；旧快照没有序号，不推断其阶段。
function rootTask(session: SessionDto, taskId: string): string {
  let root = taskId;
  const seen: string[] = [];
  while (root && !seen.includes(root)) {
    seen.push(root);
    const parent = session.tasks[root]?.parent_id;
    if (!parent) { break; }
    root = parent;
  }
  return root;
}

export function conversationRows(session: SessionDto): ConversationRow[] {
  const rows: ConversationRow[] = [];
  const events: ProcessEntry[] = [];
  for (const message of session.messages) { events.push({ sequence: message.sequence ?? 0, message: message }); }
  for (const observation of session.observations ?? []) {
    if (observation.sequence && observation.task_id && observation.kind !== 'permission_audit') {
      events.push({ sequence: observation.sequence, observation: observation });
    }
  }
  // 历史无序号消息始终保留原始顺序，并排在新事件之前。
  events.sort((a: ProcessEntry, b: ProcessEntry) => a.sequence - b.sequence);
  const active: Record<string, ConversationRow | undefined> = {};
  const completed: Record<string, ConversationRow | undefined> = {};
  let legacy = 0;
  for (const event of events) {
    const message = event.message;
    const sourceTask = message?.task_id ?? event.observation?.task_id ?? '';
    const taskId = rootTask(session, sourceTask);
    if (taskId && (message?.phase === 'progress' || (message?.phase === 'response' && sourceTask !== taskId) || event.observation)) {
      let row = event.observation?.late_result ? completed[taskId] : active[taskId];
      if (!row) {
        row = { key: `process:${event.sequence}`, entries: [], complete: false, taskId: taskId };
        active[taskId] = row;
        rows.push(row);
      }
      row.entries?.push(event);
    } else if (message) {
      if (taskId && message.phase === 'response' && active[taskId]) {
        const finished = active[taskId];
        if (finished) { finished.complete = true; completed[taskId] = finished; }
        active[taskId] = undefined;
      }
      rows.push({ key: event.sequence ? `message:${event.sequence}` : `legacy:${legacy++}`, message: message });
    }
  }
  return rows;
}
