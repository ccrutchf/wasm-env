# numpy (the manylinux wheel from PyPI, with its bundled OpenBLAS) under
# wasm-env, compared with qemu by tests/ubuntu.sh. Floating-point results
# are rounded, so the comparison doesn't depend on BLAS summation order.
import numpy as np

print("numpy", np.__version__)
a = np.arange(12, dtype=np.int64).reshape(3, 4)
print(a * 2 + 1, a.sum(), a.sum(axis=0), a.max(axis=1), a.T.shape)
x = np.linspace(0.0, 1.0, 9)
print(np.round(np.sin(x) ** 2 + np.cos(x) ** 2, 12), np.round(np.exp(x).sum(), 10))
print(np.round(np.sqrt(np.array([2.0, 9.0, 1e-8])), 12), np.round(np.log1p(x[:3]), 12))
m = np.array([[4.0, 1.0, 2.0], [1.0, 3.0, 0.5], [2.0, 0.5, 5.0]])
print("matmul", np.round(m @ m, 10))
print("solve", np.round(np.linalg.solve(m, np.array([1.0, 2.0, 3.0])), 10))
print("inv", np.round(np.linalg.inv(m) @ m, 10) + 0.0)  # + 0.0: -0.0 -> 0.0
print("det", round(float(np.linalg.det(m)), 10), "eig", np.round(np.sort(np.linalg.eigvalsh(m)), 8))
big = np.arange(200 * 150, dtype=np.float64).reshape(200, 150) / 1000.0
print("gemm", np.round((big @ big.T).trace(), 6), np.round(np.dot(big[0], big[1]), 8))
f32 = big.astype(np.float32)
print("sgemm", np.round(float((f32 @ f32.T).sum()), -3))
print("fft", np.round(np.abs(np.fft.fft(np.array([1.0, 2.0, 1.0, -1.0, 1.5, 0.0, 0.0, 2.0]))), 10))
rng = np.random.default_rng(12345)
print("random", rng.integers(0, 100, 8), np.round(rng.standard_normal(3), 12))
s = np.array(["b", "a", "c"]); print(np.sort(s), np.unique(np.array([3, 1, 3, 2, 1])))
print("bool", (a > 5).sum(), np.where(a % 3 == 0, a, -1))
print("float16", np.array([1.5, 2.25, 65504.0], dtype=np.float16) * 2)
print("complex", np.round(np.array([1 + 2j, 3 - 1j]) ** 2, 12))
print("cumsum", np.cumsum(np.arange(10)), np.round(np.mean(x), 12), np.round(np.std(x), 12))
r = (np.arange(200)[::-1] * 7 % 13)
print("sort", np.sort(r.astype(np.float64))[::40], np.sort(r)[-5:], np.sort(r.astype(np.int32))[95:100])
