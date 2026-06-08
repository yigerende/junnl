#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
junnl 粘性会话「绑定号满载过载」端到端真实测试（方案 §3.6.4 第二阶段）。

覆盖 e2e_concurrency_test.py 没测到的场景：会话亲和命中、但绑定号 max_concurrency 已满载时：
  T1  等待预算内原号释放 -> 仍用原号（保缓存，sessionAffinityHit=True）
  T2  超时仍满载        -> 放弃亲和换号 + 重绑到新号
  T3  等待数达阈值(2)    -> 第 3 个等待者立即放弃换号，不再排队
  T4  过载期间 waiting 计数在快照中可见

针对正在运行的正式服务，真实调用上游。结束后还原 max_concurrency=0、模式不变。
"""
import sys, json, time, threading
import urllib.request, urllib.error

BASE = "http://127.0.0.1:8991"
ADMIN_KEY = "sk-admin-your-secret-key"
API_KEY = "sk-kiro-rs-qazWSXedcRFV123456"
MODEL = "claude-sonnet-4.5"

PASS, FAIL = [], []
def ok(name, cond, detail=""):
    (PASS if cond else FAIL).append(name)
    mark = "PASS" if cond else "FAIL"
    print(f"  [{mark}] {name}" + (f"  -- {detail}" if detail else ""))
    return cond

def admin_get(path):
    req = urllib.request.Request(BASE + path, headers={"x-api-key": ADMIN_KEY})
    with urllib.request.urlopen(req, timeout=30) as r:
        return json.loads(r.read())

def admin_post(path, body):
    data = json.dumps(body).encode()
    req = urllib.request.Request(BASE + path, data=data,
        headers={"x-api-key": ADMIN_KEY, "content-type": "application/json"}, method="POST")
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            return r.status, json.loads(r.read())
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read())

def set_mode(mode):
    data = json.dumps({"mode": mode}).encode()
    req = urllib.request.Request(BASE + "/api/admin/config/load-balancing", data=data,
        headers={"x-api-key": ADMIN_KEY, "content-type": "application/json"}, method="PUT")
    with urllib.request.urlopen(req, timeout=30) as r:
        return json.loads(r.read())

def cred_map():
    return {c["id"]: c for c in admin_get("/api/admin/credentials")["credentials"]}

def active_of(i):  return cred_map()[i]["activeConcurrency"]
def waiting_of(i): return cred_map()[i]["waitingConcurrency"]

def wait_active_zero(timeout=40):
    t0 = time.time()
    while time.time() - t0 < timeout:
        if all(c["activeConcurrency"] == 0 for c in cred_map().values()):
            return True
        time.sleep(0.3)
    return False

def call(text, max_tokens=8, stream=False, conv_id=None, timeout=120):
    body = {"model": MODEL, "max_tokens": max_tokens, "stream": stream,
            "messages": [{"role": "user", "content": text}]}
    headers = {"x-api-key": API_KEY, "content-type": "application/json"}
    if conv_id:
        headers["x-conversation-id"] = conv_id
    data = json.dumps(body).encode()
    req = urllib.request.Request(BASE + "/v1/messages", data=data, headers=headers, method="POST")
    t0 = time.time()
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            raw = r.read().decode("utf-8", "replace")
            return r.status, (time.time()-t0)*1000, raw
    except urllib.error.HTTPError as e:
        return e.code, (time.time()-t0)*1000, e.read().decode("utf-8","replace")

def last_log(conv_id=None):
    """返回最近一条（可选按 conversationId 过滤）调用日志 (credentialId, sessionAffinityHit)."""
    d = admin_get("/api/admin/call-logs?limit=20")
    for log in d.get("logs", []):
        if conv_id is None or log.get("conversationId") == _norm_conv(conv_id):
            return log.get("credentialId"), log.get("sessionAffinityHit")
    return None, None

def _norm_conv(conv_id):
    # x-conversation-id 非 UUID 时服务端会 v5 哈希，这里无法预知最终值，故按时间序取最近一条即可
    return None

def occupy(cred_label, hold_event, started_sem, conv_id=None, max_tokens=512):
    """占住一个流式请求，慢读保持在途，直到 hold_event 触发。"""
    body = {"model": MODEL, "max_tokens": max_tokens, "stream": True,
            "messages": [{"role": "user", "content": "write a very long story, keep going and going"}]}
    headers = {"x-api-key": API_KEY, "content-type": "application/json"}
    if conv_id:
        headers["x-conversation-id"] = conv_id
    data = json.dumps(body).encode()
    req = urllib.request.Request(BASE + "/v1/messages", data=data, headers=headers, method="POST")
    try:
        r = urllib.request.urlopen(req, timeout=180)
        started_sem.release()
        while not hold_event.is_set():
            chunk = r.read(16)
            if not chunk:
                break
            time.sleep(0.05)
        r.close()
    except Exception:
        started_sem.release()


def main():
    print("="*70)
    print("粘性会话「绑定号满载过载」真实端到端测试")
    print("="*70)

    ids = sorted(cred_map().keys())
    if len(ids) < 2:
        print("需要至少 2 个可用凭据"); sys.exit(1)
    orig_mode = admin_get("/api/admin/config/load-balancing")["mode"]

    # 前提：balanced 模式（粘性仅在 balanced 生效）
    set_mode("balanced")
    # 全部不限，先归零
    admin_post("/api/admin/credentials/concurrency/batch", {"ids": ids, "maxConcurrency": 0})
    wait_active_zero()
    print(f"凭据={ids} 模式=balanced（结束还原 max=0，模式还原 {orig_mode}）\n")

    # ---------------------------------------------------------------
    # 先建立一个会话绑定：用一个独特 conv 发一次请求，记下它绑到哪个号
    # ---------------------------------------------------------------
    print("准备：建立会话绑定")
    conv = "sticky-overload-%d" % int(time.time())
    s, _, _ = call("hi", conv_id=conv)
    bound_cid, hit0 = last_log()
    ok("P0 初次请求成功并建立绑定", s == 200 and bound_cid in ids, f"bound_cid={bound_cid}")
    other = [i for i in ids if i != bound_cid][0]
    print(f"    会话 {conv} 绑定到 #{bound_cid}，另一个号 #{other}\n")
    wait_active_zero()

    # 给绑定号设上限 = 1（让它容易满载）；另一个号也设 1，便于观察
    admin_post(f"/api/admin/credentials/{bound_cid}/concurrency", {"maxConcurrency": 1})
    admin_post(f"/api/admin/credentials/{other}/concurrency", {"maxConcurrency": 1})

    # ===============================================================
    # T1: 绑定号满载，但在等待预算(2s)内释放 -> 等到后仍用原号（保缓存）
    # ===============================================================
    print("T1: 绑定号满载 + 预算内释放 -> 仍用原号")
    hold = threading.Event(); started = threading.Semaphore(0)
    # 占住绑定号（非该会话的请求也会被 least-active 分配；为确保占的是 bound_cid，
    # 我们禁用 other，使新流式请求只能落到 bound_cid）
    admin_post(f"/api/admin/credentials/{other}/disabled", {"disabled": True})
    th = threading.Thread(target=occupy, args=("bound", hold, started), daemon=True)
    th.start(); started.acquire(timeout=30); time.sleep(1.0)
    ok("T1a 绑定号已被占满 active>=1", active_of(bound_cid) >= 1, f"active={active_of(bound_cid)}")

    # 1.2s 后释放占位（在 2s 预算内），让等待中的粘性请求抢到原号
    def release_after():
        time.sleep(1.2); hold.set()
    threading.Thread(target=release_after, daemon=True).start()

    # 重新启用 other（保证“可换号”是存在的，但我们期望它仍等原号而非立刻换）
    admin_post(f"/api/admin/credentials/{other}/disabled", {"disabled": False})

    s, ms, _ = call("continue", conv_id=conv, timeout=30)
    cid_after, hit_after = last_log()
    ok("T1b 预算内释放后仍命中原号", s == 200 and cid_after == bound_cid,
       f"status={s} cid={cid_after} expect={bound_cid} waited_ms={ms:.0f}")
    ok("T1c 该请求标记 sessionAffinityHit=True", hit_after is True, f"hit={hit_after}")
    th.join(timeout=5); wait_active_zero()

    # ===============================================================
    # T2: 绑定号满载且持续不释放 -> 超时(2s)后放弃亲和，换到另一个号并重绑
    # ===============================================================
    print("\nT2: 绑定号满载且不释放 -> 超时换号 + 重绑")
    hold2 = threading.Event(); started2 = threading.Semaphore(0)
    # 占满绑定号（禁用 other 确保占位落在 bound_cid，占上后再启用 other 作为换号目标）
    admin_post(f"/api/admin/credentials/{other}/disabled", {"disabled": True})
    th2 = threading.Thread(target=occupy, args=("bound", hold2, started2), daemon=True)
    th2.start(); started2.acquire(timeout=30); time.sleep(1.0)
    admin_post(f"/api/admin/credentials/{other}/disabled", {"disabled": False})
    ok("T2a 绑定号占满、另一号可用", active_of(bound_cid) >= 1 and not cred_map()[other]["disabled"],
       f"bound active={active_of(bound_cid)}")

    s, ms, _ = call("again please", conv_id=conv, timeout=30)
    cid2, hit2 = last_log()
    ok("T2b 超时后换到另一个号 #%d" % other, s == 200 and cid2 == other,
       f"status={s} cid={cid2} waited_ms={ms:.0f}（应≈2s 等待预算后换号）")
    ok("T2c 换号请求 sessionAffinityHit=False（新号非绑定命中）", hit2 is False, f"hit={hit2}")

    # 重绑验证：现在释放原占位，再用同会话请求，应稳定在新号 other（已重绑）
    hold2.set(); th2.join(timeout=5); wait_active_zero()
    s, _, _ = call("after rebind", conv_id=conv, timeout=30)
    cid3, hit3 = last_log()
    ok("T2d 换号后会话已重绑到新号（再请求命中 #%d 且 affinityHit=True）" % other,
       s == 200 and cid3 == other and hit3 is True, f"cid={cid3} hit={hit3}")
    wait_active_zero()

    # ===============================================================
    # T3: 等待数达阈值(STICKY_MAX_WAITING=2) -> 第 3 个等待者立即放弃换号
    # 设计：把"当前绑定号"(此刻是 other) 设上限1并占满且不释放；
    #       同会话并发发 3 个请求：前 2 个进入等待(waiting<=2)，第 3 个因 waiting>=2 立即换号。
    #       为有“可换的号”，启用 bound_cid 作为换号目标。
    # ===============================================================
    print("\nT3: 等待者达阈值(2) -> 第 3 个立即放弃亲和换号")
    cur_bound = other  # T2 后会话绑定到 other
    spare = bound_cid
    # 绑定号上限1，spare 上限设大（不限）作为换号承接
    admin_post(f"/api/admin/credentials/{cur_bound}/concurrency", {"maxConcurrency": 1})
    admin_post(f"/api/admin/credentials/{spare}/concurrency", {"maxConcurrency": 0})
    wait_active_zero()

    # 占满 cur_bound：禁用 spare 确保占位落在 cur_bound，占上后启用 spare
    holdT3 = threading.Event(); startedT3 = threading.Semaphore(0)
    admin_post(f"/api/admin/credentials/{spare}/disabled", {"disabled": True})
    thT3 = threading.Thread(target=occupy, args=("curbound", holdT3, startedT3), daemon=True)
    thT3.start(); startedT3.acquire(timeout=30); time.sleep(1.0)
    admin_post(f"/api/admin/credentials/{spare}/disabled", {"disabled": False})
    ok("T3a 当前绑定号 #%d 占满、备用号 #%d 可用" % (cur_bound, spare),
       active_of(cur_bound) >= 1, f"active={active_of(cur_bound)}")

    # 同会话并发 3 个请求；采样 waiting 峰值
    results = [None]*3
    peak_wait = {"v": 0}
    stop_sampler = threading.Event()
    def sampler():
        while not stop_sampler.is_set():
            peak_wait["v"] = max(peak_wait["v"], waiting_of(cur_bound))
            time.sleep(0.02)
    smp = threading.Thread(target=sampler, daemon=True); smp.start()

    def one(i): results[i] = call("concurrent same conv #%d" % i, conv_id=conv, timeout=30)
    ths = [threading.Thread(target=one, args=(i,)) for i in range(3)]
    for t in ths: t.start()
    # 让 3 个请求都进入「试抢/等待」后，释放占位，使排队者能在超时前抢到原号
    time.sleep(1.0); holdT3.set()
    for t in ths: t.join()
    stop_sampler.set(); smp.join(timeout=2)
    thT3.join(timeout=5)

    statuses = [r[0] for r in results]
    cids = []
    for log in admin_get("/api/admin/call-logs?limit=10")["logs"]:
        cids.append(log.get("credentialId"))
    used_spare = any(c == spare for c in cids[:5])
    ok("T3b 3 个并发同会话请求全部成功", all(s == 200 for s in statuses), f"statuses={statuses}")
    ok("T3c 等待者峰值 waiting <= STICKY_MAX_WAITING(2)", peak_wait["v"] <= 2,
       f"peak_waiting={peak_wait['v']}（超过 2 说明阈值闸失效）")
    ok("T3d 超阈值的请求改用备用号 #%d（发生换号，未全部死等原号）" % spare, used_spare,
       f"recent cids={cids[:5]}")
    wait_active_zero()

    # ===============================================================
    # T4: 过载期间 waiting 计数在快照中可见（前端显示依据）
    # 复用：占满当前绑定号不释放，发一个同会话请求进入等待，采样 waiting>=1
    # ===============================================================
    print("\nT4: 过载期间 waiting 计数在快照可见")
    cur_bound2 = cur_bound
    admin_post(f"/api/admin/credentials/{cur_bound2}/concurrency", {"maxConcurrency": 1})
    admin_post(f"/api/admin/credentials/{spare}/disabled", {"disabled": True})  # 禁用换号目标，强制排队
    wait_active_zero()
    holdT4 = threading.Event(); startedT4 = threading.Semaphore(0)
    # 先禁用 spare 已做；占位落在 cur_bound2
    thT4 = threading.Thread(target=occupy, args=("cb2", holdT4, startedT4), daemon=True)
    thT4.start(); startedT4.acquire(timeout=30); time.sleep(1.0)

    saw_waiting = {"v": False}
    def probe():
        # 同会话请求：原号满 + 无可换号(spare禁用) -> 进入等待，最终超时(返回 429 或换号失败)
        call("waiting probe", conv_id=conv, timeout=30)
    pth = threading.Thread(target=probe, daemon=True); pth.start()
    t0 = time.time()
    while time.time() - t0 < 3:
        if waiting_of(cur_bound2) >= 1:
            saw_waiting["v"] = True; break
        time.sleep(0.03)
    holdT4.set(); pth.join(timeout=10); thT4.join(timeout=5)
    ok("T4a 过载等待期间 waiting>=1 在快照可见", saw_waiting["v"],
       f"saw_waiting={saw_waiting['v']}")

    # 还原 spare 可用
    admin_post(f"/api/admin/credentials/{spare}/disabled", {"disabled": False})

    # ---------------------------------------------------------------
    print("\n收尾还原")
    admin_post("/api/admin/credentials/concurrency/batch", {"ids": ids, "maxConcurrency": 0})
    for i in ids:
        if cred_map()[i]["disabled"]:
            admin_post(f"/api/admin/credentials/{i}/disabled", {"disabled": False})
    set_mode(orig_mode)
    z = wait_active_zero()
    cm = cred_map()
    ok("Z1 还原后 active 全部归零", z, str({i: cm[i]["activeConcurrency"] for i in ids}))
    ok("Z2 max 全部还原为 0", all(cm[i]["maxConcurrency"] == 0 for i in ids))
    ok("Z3 无凭据残留禁用", all(not cm[i]["disabled"] for i in ids))
    ok("Z4 模式已还原", admin_get("/api/admin/config/load-balancing")["mode"] == orig_mode)

    print("\n" + "="*70)
    print(f"结果: {len(PASS)} PASS / {len(FAIL)} FAIL")
    if FAIL:
        print("失败项:")
        for f in FAIL: print("   - " + f)
        sys.exit(1)
    print("全部通过 [OK]")

if __name__ == "__main__":
    main()
