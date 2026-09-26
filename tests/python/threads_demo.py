# Threads in Python: threading with locks, events, conditions and queues,
# thread pools, thread-local data, and a C extension (hashlib) releasing the
# GIL. The output doesn't depend on scheduling.
import concurrent.futures
import hashlib
import queue
import threading

# Many threads updating shared state under a lock.
counter = 0
lock = threading.Lock()


def add(n):
    global counter
    for _ in range(n):
        with lock:
            counter += 1


threads = [threading.Thread(target=add, args=(2000,)) for _ in range(8)]
for t in threads:
    t.start()
for t in threads:
    t.join()
print("locked counter:", counter)

# Producers and consumers through a queue.
q = queue.Queue(maxsize=4)
results = []


def produce(k):
    for i in range(100):
        q.put(k * 1000 + i)


def consume():
    while True:
        item = q.get()
        if item is None:
            break
        results.append(item)


consumers = [threading.Thread(target=consume) for _ in range(3)]
producers = [threading.Thread(target=produce, args=(k,)) for k in range(4)]
for t in consumers + producers:
    t.start()
for t in producers:
    t.join()
for _ in consumers:
    q.put(None)
for t in consumers:
    t.join()
print("queue items:", len(results), "sum:", sum(results))

# An event and a condition.
ready = threading.Event()
cond = threading.Condition()
state = []


def waiter():
    ready.wait()
    with cond:
        state.append("woken")
        cond.notify()


t = threading.Thread(target=waiter)
t.start()
with cond:
    ready.set()
    cond.wait_for(lambda: state)
t.join()
print("event and condition:", state)

# Thread-local data.
local = threading.local()


def remember(k):
    local.value = k
    return local.value * 10


with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
    print("thread pool:", sorted(pool.map(remember, range(10))))

# hashlib releases the GIL for large inputs, so these run in parallel.
data = [bytes([i]) * (1 << 20) for i in range(8)]
with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
    digests = list(pool.map(lambda d: hashlib.sha256(d).hexdigest()[:12], data))
print("sha256 in parallel:", digests)
print("threads left:", threading.active_count())
