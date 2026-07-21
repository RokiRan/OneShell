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
  metas.delete(shellId);
  aiCmds.delete(shellId);
}

export interface TermMeta {
  cwd: string;
  lastCmd: string;
}

const metas = new Map<string, TermMeta>();
/** AI 建议并写入终端的命令, 失败时用于闭环跟进 (一次性) */
const aiCmds = new Map<string, Set<string>>();

export function setTermMeta(shellId: string, meta: TermMeta) {
  metas.set(shellId, meta);
}

export function getTermMeta(shellId: string): TermMeta {
  return metas.get(shellId) ?? { cwd: "", lastCmd: "" };
}

export function markAiCommand(shellId: string, cmd: string) {
  let set = aiCmds.get(shellId);
  if (!set) {
    set = new Set();
    aiCmds.set(shellId, set);
  }
  const firstLine = cmd.trim().split("\n")[0];
  if (firstLine) set.add(firstLine);
}

/** 该命令若是 AI 写入的则取出并返回 true (一次性消费) */
export function takeAiCommand(shellId: string, cmd: string): boolean {
  const set = aiCmds.get(shellId);
  if (!set) return false;
  const t = cmd.trim();
  for (const c of set) {
    // 精确匹配或用户追加参数 (c + 空格), 避免 rm file 误吃 rm file-prod
    if (t === c || t.startsWith(c + " ")) {
      set.delete(c);
      return true;
    }
  }
  return false;
}
