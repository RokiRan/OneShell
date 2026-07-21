import { reactive } from "vue";

export interface UiMsg {
  role: "user" | "assistant";
  content: string;
}

/**
 * 每个终端标签一份独立的 AI 对话 (内存级, 标签关闭即销毁)。
 * 数组本身就是 reactive 的: 后台流式输出直接写入对应标签的数组,
 * 即使该标签当前不可见, 切回时内容也是完整的。
 */
const conversations = new Map<string, UiMsg[]>();

/** 每个标签当前进行中的 AI 请求 requestId (驱动 sending 状态) */
export const activeRequests = reactive(new Map<string, string>());

export function getConversation(shellId: string): UiMsg[] {
  let conv = conversations.get(shellId);
  if (!conv) {
    conv = reactive([]) as UiMsg[];
    conversations.set(shellId, conv);
  }
  return conv;
}

export function dropConversation(shellId: string) {
  conversations.delete(shellId);
  activeRequests.delete(shellId);
}
