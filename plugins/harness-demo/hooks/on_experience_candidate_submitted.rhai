// on_experience_candidate_submitted: 观察 hook — 演示 experience_get_candidate
log_info("[demo] on_experience_candidate_submitted: candidate submitted");

// 演示 experience_get_candidate（id 为空时返回空，仅展示 API 调用方式）
let candidate = experience_get_candidate("");
log_info(`[demo] experience_get_candidate("") = ${candidate}`);
