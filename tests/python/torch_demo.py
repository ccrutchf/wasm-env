# PyTorch on CPU: a small model's forward pass, a convolutional network,
# and a few training steps (autograd, SGD), with fixed seeds so the output
# is the same everywhere. With --onednn, oneDNN runs the convolutions and
# matmuls, with kernels it generates at run time (Xbyak), which wasm-env
# translates when they're first called; without, ATen's own kernels.
import sys

import torch
import torch.nn as nn
import torch.nn.functional as F

torch.backends.mkldnn.enabled = "--onednn" in sys.argv
torch.manual_seed(0)
torch.set_num_threads(1)

mlp = nn.Sequential(nn.Linear(8, 16), nn.ReLU(), nn.Linear(16, 4))
x = torch.randn(5, 8)
y = mlp(x)
print("mlp:", tuple(y.shape), round(y.sum().item(), 4))
print("softmax rows sum to 1:", torch.allclose(F.softmax(y, dim=1).sum(1), torch.ones(5)))

class Net(nn.Module):
    def __init__(self):
        super().__init__()
        self.conv = nn.Conv2d(1, 4, 3, padding=1)
        self.bn = nn.BatchNorm2d(4)
        self.fc = nn.Linear(4 * 4 * 4, 10)

    def forward(self, x):
        x = F.max_pool2d(F.relu(self.bn(self.conv(x))), 2)
        return self.fc(x.flatten(1))

net = Net()
images = torch.randn(3, 1, 8, 8)
logits = net(images)
print("cnn:", tuple(logits.shape), round(logits.abs().sum().item(), 4))

# Training: fit the MLP to a target for a few steps; the loss falls.
target = torch.randn(5, 4)
opt = torch.optim.SGD(mlp.parameters(), lr=0.1)
losses = []
for _ in range(20):
    opt.zero_grad()
    loss = F.mse_loss(mlp(x), target)
    loss.backward()
    opt.step()
    losses.append(loss.item())
print("training: loss %.4f -> %.4f" % (losses[0], losses[-1]))

with torch.no_grad():
    a = torch.randn(64, 64)
    print("matmul trace: %.4f" % (a @ a.T).trace().item())
    print("eig of a symmetric matrix:", round(torch.linalg.eigvalsh(a @ a.T)[-1].item(), 3))
