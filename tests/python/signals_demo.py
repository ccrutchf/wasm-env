import os, signal, time
got = []
signal.signal(signal.SIGUSR1, lambda s, f: got.append(s))
os.kill(os.getpid(), signal.SIGUSR1)
print("handler ran:", got)
try:
    os.kill(os.getpid(), signal.SIGINT)
    time.sleep(0.01)
    print("not interrupted")
except KeyboardInterrupt:
    print("KeyboardInterrupt")
# A handler that raises ends a blocking read (Python retries reads a
# handler returns from normally, PEP 475).
def stop(s, f):
    raise RuntimeError("stopped by signal %d" % s)
signal.signal(signal.SIGUSR1, stop)
r, w = os.pipe()
pid = os.fork()
if pid == 0:
    time.sleep(0.05)
    os.kill(os.getppid(), signal.SIGUSR1)
    os._exit(0)
try:
    os.read(r, 1)
except RuntimeError as e:
    print(e)
os.waitpid(pid, 0)
signal.signal(signal.SIGALRM, lambda s, f: print("alarm"))
signal.setitimer(signal.ITIMER_REAL, 0.05)
time.sleep(0.2)
print("done")
