"""Prompt sizes of loadgen's profile mode in the tokens a real model counts.

Needs the tokenizers package and QWEN_TOKENIZER, the path of the
tokenizer.json of Qwen/Qwen2.5-7B-Instruct; CI fetches it at a pinned
revision. The NATO vocabulary loadgen used before fails here: its words are
1.72 Qwen tokens each on average, so a GPU served prompts 1.7 times the
profile's context_tokens.
"""
import os
import sys
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import loadgen  # noqa: E402
from tokenizers import Tokenizer  # noqa: E402

PROFILE = HERE / "profiles" / "azure-conv-2023-peak.csv"


class PromptTokens(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tok = Tokenizer.from_file(os.environ["QWEN_TOKENIZER"])

    def count(self, text):
        return len(self.tok.encode(text, add_special_tokens=True).ids)

    def test_every_word_is_one_token_first_and_after_a_space(self):
        for w in loadgen.VOCAB:
            self.assertEqual((self.count(w), self.count(" " + w)), (1, 1), w)

    def test_profile_prompts_are_context_tokens_long(self):
        reqs, _, _ = loadgen.profile_requests(str(PROFILE), 0.025, 42)
        self.assertEqual(len(reqs), 68)
        for r in reqs:
            self.assertEqual(self.count(r["prompt"]), r["context_tokens"], r["trace_line"])


if __name__ == "__main__":
    unittest.main()
