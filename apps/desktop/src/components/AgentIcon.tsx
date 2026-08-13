import claudeIconUrl from "../../../../assets/branding/agents/anthropic/ClaudeIcon-Rounded.svg";
import codexIconUrl from "../../../../assets/branding/agents/openai/OAI_OpenAI-Blossom_White.svg";

import type { RunningAgent } from "../lib/contracts";

type AgentIconProps = {
  agent: RunningAgent["agent"];
};

export function AgentIcon({ agent }: AgentIconProps) {
  const isCodex = agent === "codex";
  return (
    <span className={`agent-product-icon agent-product-icon--${agent}`} aria-hidden="true">
      <img alt="" draggable={false} src={isCodex ? codexIconUrl : claudeIconUrl} />
    </span>
  );
}
