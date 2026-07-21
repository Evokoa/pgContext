import unittest

import benchmark


class BenchmarkMathTests(unittest.TestCase):
    def test_percentile_uses_linear_interpolation(self):
        self.assertEqual(benchmark.percentile([1.0, 2.0, 3.0, 4.0], 50), 2.5)
        self.assertEqual(benchmark.percentile([1.0, 2.0, 3.0, 4.0], 95), 3.85)

    def test_recall_at_k_counts_overlap(self):
        exact = [[1, 2, 3], [4, 5, 6]]
        approximate = [[1, 3, 8], [4, 5, 6]]
        self.assertAlmostEqual(benchmark.mean_recall_at_k(exact, approximate, 3), 5 / 6)

    def test_vector_literal_is_stable_and_finite(self):
        self.assertEqual(benchmark.vector_literal([0.25, -1.0, 0.0]), "[0.25,-1,0]")
        with self.assertRaises(ValueError):
            benchmark.vector_literal([float("nan")])

    def test_selectivity_lanes_cover_expected_fractions(self):
        fractions = sorted(
            1 / modulo for _, modulo in benchmark.SELECTIVITY_LANES.values()
        )
        self.assertEqual(fractions, [0.01, 0.1, 0.5])
        for column, modulo in benchmark.SELECTIVITY_LANES.values():
            self.assertEqual(benchmark.FILTER_MODULO[column], modulo)

    def test_query_sql_parameterizes_filter_column(self):
        self.assertIn("WHERE bucket_100 = %s", benchmark.query_sql("pgvector", True, "bucket_100"))
        self.assertNotIn("WHERE", benchmark.query_sql("pgvector", True, None))

    def test_synthetic_corpus_is_normalized_and_seeded(self):
        import tempfile
        from pathlib import Path

        import numpy as np

        with tempfile.TemporaryDirectory() as tmp:
            metadata = benchmark.prepare_synthetic_data(Path(tmp), 2000)
            corpus = np.load(Path(tmp) / "corpus.npy")
            queries = np.load(Path(tmp) / "queries.npy")
            self.assertEqual(metadata["corpus_rows"], 2000)
            self.assertEqual(corpus.shape, (2000, benchmark.MODEL_DIMENSIONS))
            self.assertEqual(queries.shape[1], benchmark.MODEL_DIMENSIONS)
            norms = np.linalg.norm(corpus, axis=1)
            self.assertTrue(np.allclose(norms, 1.0, atol=1e-5))
            metadata_again = benchmark.prepare_synthetic_data(Path(tmp), 2000)
            corpus_again = np.load(Path(tmp) / "corpus.npy")
            self.assertEqual(metadata, metadata_again)
            self.assertTrue(np.array_equal(corpus, corpus_again))

    def test_qdrant_effective_candidates_scales_by_segment_count(self):
        self.assertEqual(benchmark.qdrant_effective_candidates(384, 10), 3840)
        self.assertEqual(benchmark.qdrant_effective_candidates(128, 1), 128)
        # A missing or zero segment count must not zero the effort estimate.
        self.assertEqual(benchmark.qdrant_effective_candidates(256, 0), 256)

    def test_qdrant_effort_metadata_records_segments_and_totals(self):
        class FakeInfo:
            segments_count = 8

        class FakeClient:
            def get_collection(self, _name):
                return FakeInfo()

        payload = benchmark.qdrant_effort_metadata(FakeClient(), [128, 384])
        self.assertEqual(payload["segments_count"], 8)
        self.assertEqual(payload["effective_candidates"], {"128": 1024, "384": 3072})

    def test_qdrant_effort_metadata_degrades_to_recorded_error(self):
        class DownClient:
            def get_collection(self, _name):
                raise RuntimeError("connection refused")

        payload = benchmark.qdrant_effort_metadata(DownClient(), [128])
        self.assertIsNone(payload["segments_count"])
        self.assertIn("connection refused", payload["capture_error"])


if __name__ == "__main__":
    unittest.main()
