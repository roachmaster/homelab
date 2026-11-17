"""Offline admission for the pinned GPT-OSS 120B Mail profile."""

from dataclasses import dataclass
from datetime import datetime, timezone
from functools import lru_cache


PROFILE_ID = "gpt-oss-120b-harmony-v1"


@dataclass(frozen=True)
class MailModelProfile:
    context_tokens: int = 4096
    completion_tokens: int = 1024
    margin_tokens: int = 16
    min_followup_tokens: int = 256


model_profile = MailModelProfile()


class TokenizerUnavailable(RuntimeError):
    """The runner could not load or execute the pinned Harmony encoding."""


@lru_cache(maxsize=1)
def harmony_renderer():
    try:
        from openai_harmony import (
            Conversation,
            HarmonyEncodingName,
            Message,
            ReasoningEffort,
            Role,
            SystemContent,
            load_harmony_encoding,
        )

        encoding = load_harmony_encoding(HarmonyEncodingName.HARMONY_GPT_OSS)
    except (ImportError, OSError, RuntimeError, ValueError) as error:
        raise TokenizerUnavailable("GPT-OSS 120B tokenizer unavailable") from error

    def render(messages, utc_date):
        system = (SystemContent.new().with_reasoning_effort(ReasoningEffort.LOW)
                  .with_conversation_start_date(utc_date))
        turns = [Message.from_role_and_content(Role.SYSTEM, system)]
        for message in messages:
            role = Role(message["role"])
            turn = Message.from_role_and_content(role, message["content"])
            turns.append(turn.with_channel("final") if role == Role.ASSISTANT else turn)
        tokens = encoding.render_conversation_for_completion(
            Conversation.from_messages(turns), Role.ASSISTANT,
        )
        return len(tokens)

    return render


def prompt_tokens(messages):
    """Count the system message, completed exchanges and next assistant prefix."""
    utc_date = datetime.now(timezone.utc).date().isoformat()
    try:
        return harmony_renderer()(messages, utc_date)
    except (OSError, RuntimeError, ValueError) as error:
        raise TokenizerUnavailable("GPT-OSS 120B prompt could not be sized") from error
