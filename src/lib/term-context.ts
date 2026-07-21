/**
 * 每个终端的滚动输出缓冲, 供 AI 面板读取上下文。
 * 只保留尾部 12KB, 并剥掉 ANSI 转义序列。
 */
const buffers = new Map<string, string>();

const ANSI_RE =
  // eslint-disable-next-line no-control-regex
  /[\u001b\u009b][[\]()#;?]*(?:(?:(?:[a-zA-Z\d]*(?:;[a-zA-Z\d]*)*)?\u0007)|(?:(?:\d{1,4}(?:;\d{0,4})*)?[\dA-PR-TZcf-nq-uy=><~]))/g;

export function appendTermData(shellId: string, text: string) {
  const clean = text.replace(ANSI_RE, "").replace(/\r/g, "");
  const prev = buffers.get(shellId) ?? "";
  buffers.set(shellId, (prev + clean).slice(-12000));
}

export function getTermContext(shellId: string, maxChars = 6000): string {
  const buf = buffers.get(shellId) ?? "";
  return buf.slice(-maxChars);
}

export function dropTermContext(shellId: string) {
  buffers.delete(shellId);
}
