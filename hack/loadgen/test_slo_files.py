"""The objectives of one GPU class differ from slo.json only where its ADR says.

ADR-0020 changes the time per output token for one L4 running
Qwen2.5-7B-Instruct in bf16 on vLLM v0.11.0 and keeps every other objective
of ADR-0013.
"""
import json
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent


class GpuClassObjectives(unittest.TestCase):
    def test_l4_qwen7b_bf16_changes_only_tpot(self):
        base = json.loads((HERE / "slo.json").read_text())
        l4 = json.loads((HERE / "slo-l4-qwen7b-bf16.json").read_text())
        self.assertEqual(set(base), set(l4))
        self.assertEqual({k for k in base if base[k] != l4[k]}, {"doc", "tpot_ms"})
        self.assertEqual((l4["doc"], l4["tpot_ms"]), ("ADR-0020", 100))


if __name__ == "__main__":
    unittest.main()
