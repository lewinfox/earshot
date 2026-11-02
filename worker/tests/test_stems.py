"""Stems: the chunks' cross-fade weights cover every frame exactly once, wherever the
track ends (the model itself isn't run here)."""
import numpy as np
import pytest

from earshot_worker.stems import fade

SR = 100  # frames per "second", to keep the arrays small
CHUNK, OV = 60 * SR, 2 * SR


@pytest.mark.parametrize("seconds", [10, 59.99, 60, 61, 61.99, 62, 62.01, 120, 180.5, 182, 185, 243.7])
def test_chunk_weights_sum_to_one(seconds):
    n = int(seconds * SR)
    total = np.zeros(n)
    for start in range(0, n, CHUNK):
        lo, hi = max(0, start - OV), min(n, start + CHUNK + OV)
        total[lo:hi] += fade(start, lo, hi, n, CHUNK, OV)
    np.testing.assert_allclose(total, 1.0, atol=1e-6)
