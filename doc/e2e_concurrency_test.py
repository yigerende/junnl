#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
junnl 凭据并发控制方案 —— 上线前端到端测试。

对照《凭据并发控制方案.md》§八 验证要点逐项检查。
针对正在运行的正式服务（默认 127.0.0.1:8991），真实调用上游（消耗少量额度）。

用法:
    python doc/e2e_concurrency_test.py

测试结束会把所有凭据的 max_concurrency 还原为 0，并核对 active 全部归零（无泄漏）。
"""
import sys
import json
import time
import threading
import urllib.request
import urllib.error

BASE = "http://127.0.0.1:8991"
ADMIN_KEY = "sk-admin-your-secret-key"
API_KEY = "sk-kiro-rs-qazWSXedcRFV123456"
MODEL = "claude-sonnet-4.5"

PASS, FAIL = [], []


def ok(name, cond, detail=""):
    (PASS if cond else FAIL).append(name)
    mark = "\033[32mPASS\033[0m" if cond else "\033[31mFAIL\033[0m"
    print(f"  [{mark}] {name}" + (f"  — {detail}" if detail else ""))
    return cond


def admin_get(path):
    req = urllib.request.Request(BASE + path, headers={"x-api-key": ADMIN_KEY})
    with urllib.request.urlopen(req, timeout=30) as r:
        return json.loads(r.read())


def admin_post(path, body):
    data = json.dumps(body).encode()
    req = urllib.request.Request(
        BASE + path, data=data,
        headers={"x-api-key": ADMIN_KEY, "content-type": "application/json"},
        method="POST",
    )
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            return r.status, json.loads(r.read())
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read())


def snapshot():
    return admin_get("/api/admin/credentials")


def cred_map():
    return {c["id"]: c for c in snapshot()["credentials"]}


def messages_body(text="Reply with just OK", max_tokens=16, stream=False, conv_id=None):
    body = {
        "model": MODEL,
        "max_tokens": max_tokens,
        "stream": stream,
        "messages": [{"role": "user", "content": text}],
    }
    return body, conv_id


def call_messages(text="Reply with just OK", max_tokens=16, stream=False,
                  conv_id=None, path="/v1/messages", read_body=True):
    """发起一次客户端请求。返回 (status, elapsed_ms, credential_id_or_None, raw_text)."""
    body, _ = messages_body(text, max_tokens, stream, conv_id)
    headers = {"x-api-key": API_KEY, "content-type": "application/json"}
    if conv_id:
        headers["x-conversation-id"] = conv_id
    data = json.dumps(body).encode()
    req = urllib.request.Request(BASE + path, data=data, headers=headers, method="POST")
    t0 = time.time()
    try:
        with urllib.request.urlopen(req, timeout=120) as r:
            raw = r.read().decode("utf-8", "replace") if read_body else ""
            return r.status, (time.time() - t0) * 1000, raw
    except urllib.error.HTTPError as e:
        raw = e.read().decode("utf-8", "replace")
        return e.code, (time.time() - t0) * 1000, raw


def max_active_during(fn, sample_interval=0.03):
    """运行 fn() 的同时高频采样每个凭据的 active，返回 (fn结果, {id: 峰值active}, 峰值waiting)."""
    peak = {}
    peak_wait = {}
    stop = threading.Event()

    def sampler():
        while not stop.is_set():
            try:
                for c in snapshot()["credentials"]:
                    peak[c["id"]] = max(peak.get(c["id"], 0), c["activeConcurrency"])
                    peak_wait[c["id"]] = max(peak_wait.get(c["id"], 0), c["waitingConcurrency"])
            except Exception:
                pass
            time.sleep(sample_interval)

    th = threading.Thread(target=sampler, daemon=True)
    th.start()
    result = fn()
    time.sleep(0.1)
    stop.set()
    th.join(timeout=2)
    return result, peak, peak_wait


def set_all_concurrency(value):
    ids = [c["id"] for c in snapshot()["credentials"]]
    st, resp = admin_post("/api/admin/credentials/concurrency/batch",
                          {"ids": ids, "maxConcurrency": value})
    return st, resp, ids


def wait_active_zero(timeout=30):
    t0 = time.time()
    while time.time() - t0 < timeout:
        cm = cred_map()
        if all(c["activeConcurrency"] == 0 for c in cm.values()):
            return True
        time.sleep(0.3)
    return False


# ============================================================
def main():
    print("=" * 70)
    print("junnl 并发控制 端到端上线测试")
    print("=" * 70)

    base = cred_map()
    ids = sorted(base.keys())
    print(f"凭据: {ids} | 模式将分别测试 balanced / priority")
    if len(ids) < 2:
        print("需要至少 2 个可用凭据，已退出。")
        sys.exit(1)

    # 起点必须干净
    ok("E0 起点 active 全为 0", all(c["activeConcurrency"] == 0 for c in base.values()),
       str({i: base[i]["activeConcurrency"] for i in ids}))

    orig_mode = admin_get("/api/admin/config/load-balancing")["mode"]
    print(f"原负载均衡模式: {orig_mode}（测试结束会还原）\n")

    # ---------- E 配置与快照字段 ----------
    print("E. 配置与快照字段")
    s = snapshot()["credentials"][0]
    ok("E1 快照含 maxConcurrency/activeConcurrency/waitingConcurrency",
       all(k in s for k in ("maxConcurrency", "activeConcurrency", "waitingConcurrency")),
       f"keys present")
    st, resp = admin_post(f"/api/admin/credentials/{ids[0]}/concurrency", {"maxConcurrency": 5})
    ok("E2 单个设置并发 max=5 成功", st == 200 and cred_map()[ids[0]]["maxConcurrency"] == 5,
       f"status={st}")
    st, resp, _ = set_all_concurrency(3)
    cm = cred_map()
    ok("E3 批量设置并发 max=3 全部生效", st == 200 and all(cm[i]["maxConcurrency"] == 3 for i in ids),
       f"status={st} maxs={[cm[i]['maxConcurrency'] for i in ids]}")
    st, resp, _ = set_all_concurrency(0)
    ok("E4 批量还原 max=0 (不限) 生效", all(cred_map()[i]["maxConcurrency"] == 0 for i in ids))

    # ---------- D3 无状态请求摊开 (balanced) + A 防踩踏 ----------
    print("\nA/D. balanced 模式：least-active 防踩踏 + 摊开")
    set_mode("balanced")
    set_all_concurrency(0)
    wait_active_zero()

    N = 8
    def burst_no_conv():
        results = [None] * N
        def one(i):
            results[i] = call_messages(text="hi", max_tokens=8)
        ths = [threading.Thread(target=one, args=(i,)) for i in range(N)]
        for t in ths: t.start()
        for t in ths: t.join()
        return results

    res, peak, _ = max_active_during(burst_no_conv)
    statuses = [r[0] for r in res]
    spread = sum(1 for i in ids if peak.get(i, 0) > 0)
    ok("A1 并发请求全部成功 (200)", all(s == 200 for s in statuses), f"statuses={statuses}")
    ok("A2 负载摊开到 >=2 个凭据 (不踩踏单号)", spread >= 2, f"峰值active={peak}")
    ok("B-bal acquire 后 active 归零 (无泄漏)", wait_active_zero(), str(cred_map_active()))

    # ---------- B guard 生命周期：流式 ----------
    print("\nB. guard 生命周期 / 无泄漏")
    st, ms, raw = call_messages(text="count 1 to 3", max_tokens=64, stream=True)
    ok("B1 流式请求成功", st == 200 and "message_stop" in raw, f"status={st}")
    ok("B2 流式读完后 active 归零", wait_active_zero(), str(cred_map_active()))

    st, ms, raw = call_messages(text="say hi", max_tokens=16, stream=False)
    ok("B3 非流式请求成功", st == 200, f"status={st}")
    ok("B4 非流式读完后 active 归零", wait_active_zero(), str(cred_map_active()))

    # 客户端中断流：读 header 后立刻关闭连接
    ok("B5 客户端中断流后 active 归零", client_abort_then_zero(),
       str(cred_map_active()))

    # ---------- 第二层：满载 busy 429 + 短等待 ----------
    print("\n(二层) 硬上限：满载短等待 + 繁忙 429")
    set_all_concurrency(1)  # 每个号最多 1 在途
    wait_active_zero()
    # 先占满：每个号各发一个长一点的流式请求并保持在途
    busy_result = saturate_and_probe(ids)
    ok("F1 满载时新请求返回 429 繁忙", busy_result["got_429"],
       f"probe_status={busy_result['probe_status']} wait_ms={busy_result['probe_ms']:.0f}")
    ok("F2 繁忙前确有短等待(>=0.8s, 非立即失败)", busy_result["probe_ms"] >= 800,
       f"wait_ms={busy_result['probe_ms']:.0f}")
    ok("F3 占位请求各自占用 active>=1", busy_result["saturated_ok"], busy_result["detail"])
    set_all_concurrency(0)
    ok("F4 压测静置后 active 全部归零 (无累积泄漏)", wait_active_zero(), str(cred_map_active()))

    # ---------- D 粘性会话 (balanced) ----------
    print("\nD. 粘性会话 (balanced)")
    set_all_concurrency(0)
    wait_active_zero()
    conv = "e2e-sticky-conv-0001"
    s1, _, r1 = call_messages(text="hi", max_tokens=8, conv_id=conv)
    cid1 = last_log_credential()
    s2, _, r2 = call_messages(text="hi again", max_tokens=8, conv_id=conv)
    cid2 = last_log_credential()
    ok("D1 同 conversationId 连续请求绑定同一凭据", s1 == 200 and s2 == 200 and cid1 == cid2 and cid1 is not None,
       f"cid1={cid1} cid2={cid2}")

    # ---------- C priority 语义 ----------
    print("\nC. priority 模式语义")
    set_mode("priority")
    set_all_concurrency(0)
    wait_active_zero()
    # 设不同优先级：id[0]=0(高), id[1]=1(低)
    admin_post(f"/api/admin/credentials/{ids[0]}/priority", {"priority": 0})
    admin_post(f"/api/admin/credentials/{ids[1]}/priority", {"priority": 1})

    res, peak, _ = max_active_during(burst_no_conv)
    statuses = [r[0] for r in res]
    ok("C1 priority+不限: 仅最高优先级档被使用, 低档 active 恒 0",
       all(s == 200 for s in statuses) and peak.get(ids[1], 0) == 0 and peak.get(ids[0], 0) > 0,
       f"峰值active={peak}")

    # 高档设上限并打满 → 落到低档
    admin_post(f"/api/admin/credentials/{ids[0]}/concurrency", {"maxConcurrency": 1})
    wait_active_zero()
    fell = priority_fallover(ids)
    ok("C2 高优先级档满载时落到下一档", fell["used_low"], fell["detail"])
    admin_post(f"/api/admin/credentials/{ids[0]}/concurrency", {"maxConcurrency": 0})
    wait_active_zero()
    # 解除满载后回到高档
    s, _, _ = call_messages(text="hi", max_tokens=8)
    cid = last_log_credential()
    ok("C3 解除满载后回到最高优先级档", cid == ids[0], f"used cid={cid}, expect {ids[0]}")

    # ---------- F 不回归：基本功能仍正常 ----------
    print("\nF. 不回归")
    s, _, raw = call_messages(text="say hello", max_tokens=16, stream=False)
    ok("R1 非流式回归正常", s == 200)
    s, _, raw = call_messages(text="count to 3", max_tokens=48, stream=True)
    ok("R2 流式回归正常", s == 200 and "message_stop" in raw)

    # ---------- 收尾：还原 ----------
    print("\n收尾还原")
    set_all_concurrency(0)
    admin_post(f"/api/admin/credentials/{ids[0]}/priority", {"priority": base[ids[0]]["priority"]})
    admin_post(f"/api/admin/credentials/{ids[1]}/priority", {"priority": base[ids[1]]["priority"]})
    set_mode(orig_mode)
    z = wait_active_zero()
    ok("Z1 还原后 active 全部归零", z, str(cred_map_active()))
    cm = cred_map()
    ok("Z2 max_concurrency 已还原为 0", all(cm[i]["maxConcurrency"] == 0 for i in ids))
    ok("Z3 负载均衡模式已还原", admin_get("/api/admin/config/load-balancing")["mode"] == orig_mode)

    # ---------- 汇总 ----------
    print("\n" + "=" * 70)
    print(f"结果: {len(PASS)} PASS / {len(FAIL)} FAIL")
    if FAIL:
        print("失败项:")
        for f in FAIL:
            print("   - " + f)
        sys.exit(1)
    print("全部通过 [OK]")


# ---------- helpers needing app state ----------
def set_mode(mode):
    data = json.dumps({"mode": mode}).encode()
    req = urllib.request.Request(BASE + "/api/admin/config/load-balancing", data=data,
                                 headers={"x-api-key": ADMIN_KEY, "content-type": "application/json"},
                                 method="PUT")
    with urllib.request.urlopen(req, timeout=30) as r:
        return json.loads(r.read())


def cred_map_active():
    return {i: c["activeConcurrency"] for i, c in cred_map().items()}


def last_log_credential():
    """读取最近一条调用日志的 credentialId."""
    try:
        d = admin_get("/api/admin/call-logs?limit=1")
        logs = d.get("logs", [])
        if logs:
            return logs[0].get("credentialId")
    except Exception:
        pass
    return None


def client_abort_then_zero():
    """发起流式请求，读到首字节就立刻断开，验证 guard 仍释放。"""
    body = {"model": MODEL, "max_tokens": 256, "stream": True,
            "messages": [{"role": "user", "content": "write a long paragraph about the sea"}]}
    data = json.dumps(body).encode()
    req = urllib.request.Request(BASE + "/v1/messages", data=data,
                                 headers={"x-api-key": API_KEY, "content-type": "application/json"},
                                 method="POST")
    try:
        r = urllib.request.urlopen(req, timeout=120)
        r.read(64)   # 读一点点
        r.close()    # 立刻断开
    except Exception:
        pass
    return wait_active_zero(timeout=40)


def saturate_and_probe(ids):
    """每个号占 1 个长流式在途，再发一个探测请求，应得 429。"""
    n = len(ids)
    hold = threading.Event()
    started = threading.Semaphore(0)

    def occupy():
        body = {"model": MODEL, "max_tokens": 512, "stream": True,
                "messages": [{"role": "user", "content": "write a very long story, keep going"}]}
        data = json.dumps(body).encode()
        req = urllib.request.Request(BASE + "/v1/messages", data=data,
                                     headers={"x-api-key": API_KEY, "content-type": "application/json"},
                                     method="POST")
        try:
            r = urllib.request.urlopen(req, timeout=120)
            started.release()
            # 慢慢读，保持在途
            while not hold.is_set():
                chunk = r.read(16)
                if not chunk:
                    break
                time.sleep(0.05)
            r.close()
        except Exception:
            started.release()

    occupants = [threading.Thread(target=occupy, daemon=True) for _ in range(n)]
    for t in occupants:
        t.start()
    # 等占位请求都连上
    for _ in range(n):
        started.acquire(timeout=30)
    time.sleep(1.0)  # 让 active 稳定

    cm = cred_map()
    saturated_ok = all(cm[i]["activeConcurrency"] >= 1 for i in ids)
    detail = "active=" + str({i: cm[i]["activeConcurrency"] for i in ids})

    # 探测请求：应满载 → 短等待 → 429
    pstatus, pms, praw = call_messages(text="hi", max_tokens=8, stream=False)
    got_429 = pstatus == 429

    hold.set()
    for t in occupants:
        t.join(timeout=5)

    return {"got_429": got_429, "probe_status": pstatus, "probe_ms": pms,
            "saturated_ok": saturated_ok, "detail": detail, "raw": praw[:200]}


def priority_fallover(ids):
    """高档(ids[0])上限=1已设。占满高档，再发请求应落到低档(ids[1])。"""
    hold = threading.Event()
    started = threading.Semaphore(0)

    def occupy_high():
        body = {"model": MODEL, "max_tokens": 512, "stream": True,
                "messages": [{"role": "user", "content": "write a very long story, keep going"}]}
        data = json.dumps(body).encode()
        req = urllib.request.Request(BASE + "/v1/messages", data=data,
                                     headers={"x-api-key": API_KEY, "content-type": "application/json"},
                                     method="POST")
        try:
            r = urllib.request.urlopen(req, timeout=120)
            started.release()
            while not hold.is_set():
                chunk = r.read(16)
                if not chunk:
                    break
                time.sleep(0.05)
            r.close()
        except Exception:
            started.release()

    th = threading.Thread(target=occupy_high, daemon=True)
    th.start()
    started.acquire(timeout=30)
    time.sleep(1.0)
    # 此时高档应满载，新请求落到低档
    s, _, _ = call_messages(text="hi", max_tokens=8)
    cid = last_log_credential()
    hold.set()
    th.join(timeout=5)
    return {"used_low": cid == ids[1], "detail": f"new req used cid={cid}, expect low={ids[1]}"}


if __name__ == "__main__":
    main()
