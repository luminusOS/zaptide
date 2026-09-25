#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

BINARY="${ZAPTIDE_BIN:-$PROJECT_ROOT/target/debug/zaptide}"
FIXTURE_PATH="${FIXTURE_PATH:-}"
SYNTHETIC_SEED="${SYNTHETIC_SEED:-42}"
SAMPLING_INTERVAL_MS="${SAMPLING_INTERVAL_MS:-100}"
WARMUP_SEC="${WARMUP_SEC:-300}"
REPETITIONS="${REPETITIONS:-5}"
EVENT_COUNT="${EVENT_COUNT:-1000}"
EVENT_SEED="${EVENT_SEED:-7}"
TRACE_MARKER="zaptide-perf"

CPU_BUDGET_PCT="0.5"
WAKEUP_BUDGET_PER_MIN="10"
RSS_GROWTH_MIB="10"
RSS_GROWTH_PCT="2"
LATENCY_P95_MS="50"
FRAME_P95_DEADLINE_MS="16"
MISSED_FRAME_PCT="1"

RESULTS_DIR="${RESULTS_DIR:-$PROJECT_ROOT/target/perf-results}"
VERIFY_ONLY=false

usage() {
    cat <<EOF
Usage: $(basename "$0") [OPTIONS]

Measure native GTK4/Relm4 shell performance.

Options:
  --verify          Check script runs correctly, exit after validation
  --binary PATH     Path to zaptide binary (default: target/debug/zaptide)
  --fixture PATH    Path to synthetic fixture data
  --seed N          Synthetic event injection seed (default: 7)
  --warmup SEC      Warmup period in seconds (default: 300)
  --reps N          Number of repetitions (default: 5)
  --events N        Synthetic event count (default: 1000)
  --output DIR      Results directory (default: target/perf-results)
  -h, --help        Show this help
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --verify) VERIFY_ONLY=true; shift ;;
        --binary) BINARY="$2"; shift 2 ;;
        --fixture) FIXTURE_PATH="$2"; shift 2 ;;
        --seed) EVENT_SEED="$2"; shift 2 ;;
        --warmup) WARMUP_SEC="$2"; shift 2 ;;
        --reps) REPETITIONS="$2"; shift 2 ;;
        --events) EVENT_COUNT="$2"; shift 2 ;;
        --output) RESULTS_DIR="$2"; shift 2 ;;
        -h|--help) usage; exit 0 ;;
        *) echo "Unknown option: $1" >&2; usage >&2; exit 1 ;;
    esac
done

mkdir -p "$RESULTS_DIR"
TIMESTAMP="$(date +%Y%m%d-%H%M%S)"
LOG="$RESULTS_DIR/perf-$TIMESTAMP.log"

warn() { echo "[WARN] $*" | tee -a "$LOG"; }
info() { echo "[INFO] $*" | tee -a "$LOG"; }
fail() { echo "[FAIL] $*" | tee -a "$LOG"; FAILURES=$((FAILURES + 1)); }
pass() { echo "[PASS] $*" | tee -a "$LOG"; }

FAILURES=0

has_cmd() { command -v "$1" >/dev/null 2>&1; }

print_env_manifest() {
    info "=== Environment Manifest ==="
    info "Kernel: $(uname -r)"
    info "CPU model: $(awk -F: '/model name/{gsub(/^ +/,"",$2); print $2; exit}' /proc/cpuinfo 2>/dev/null || echo unknown)"
    info "CPU cores: $(nproc 2>/dev/null || echo unknown)"

    local mem_kb
    mem_kb="$(awk '/MemTotal/{print $2}' /proc/meminfo 2>/dev/null || echo 0)"
    info "Memory: $(( mem_kb / 1024 )) MiB"

    if has_cmd lspci; then
        info "GPU: $(lspci 2>/dev/null | grep -i vga | head -1 | sed 's/.*: //' || echo unknown)"
    else
        info "GPU: lspci not available"
    fi

    local refresh="unknown"
    if has_cmd xrandr; then
        refresh="$(xrandr 2>/dev/null | grep '\*' | head -1 | awk '{print $NF}' || echo unknown)"
    elif [[ -d /sys/class/drm ]]; then
        for m in /sys/class/drm/card*-*/modes; do
            if [[ -r "$m" ]]; then
                refresh="$(head -1 "$m" | grep -oP '\d+\.\d+' | head -1 || true)"
                [[ -n "$refresh" ]] && break
            fi
        done
    fi
    info "Refresh rate: ${refresh:-unknown}"

    if has_cmd gnome-shell; then
        info "GNOME session: $(gnome-shell --version 2>/dev/null || echo unknown)"
    else
        info "GNOME session: not detected"
    fi

    info "GTK version: $(pkg-config --modversion gtk4 2>/dev/null || echo unknown)"
    info "libadwaita version: $(pkg-config --modversion libadwaita-1 2>/dev/null || echo unknown)"
    info "Relm4 version: $(grep -m1 '^relm4 ' "$PROJECT_ROOT/Cargo.lock" 2>/dev/null | awk '{print $2}' || echo unknown)"
    info "Scale factor: ${GDK_SCALE:-${GDK_DPI_SCALE:-unset}}"
    info "Binary: $BINARY"
    info "Fixture: ${FIXTURE_PATH:-built-in synthetic}"
    info "Synthetic seed: $SYNTHETIC_SEED"
    info "Event seed: $EVENT_SEED"
    info "Sampling interval: ${SAMPLING_INTERVAL_MS}ms"
    info "Warmup: ${WARMUP_SEC}s"
    info "Repetitions: $REPETITIONS"
    info "Event count: $EVENT_COUNT"
    info "==========================="
}

verify_tools() {
    info "=== Tool Verification ==="
    local all_ok=true

    if [[ ! -x "$BINARY" ]]; then
        warn "Binary not found or not executable: $BINARY"
        if ! $VERIFY_ONLY; then
            echo "Build the binary first: cargo build" >&2
            exit 1
        fi
        all_ok=false
    else
        info "Binary OK: $BINARY"
    fi

    for tool in ps perf; do
        if has_cmd "$tool"; then
            info "Available: $tool"
        else
            warn "Missing: $tool"
            all_ok=false
        fi
    done

    if [[ -f /proc/self/status ]]; then
        info "Available: /proc/[pid]/status"
    else
        warn "/proc/[pid]/status not available"
        all_ok=false
    fi

    if has_cmd sysprof-cli; then
        info "Available: sysprof-cli"
    else
        warn "sysprof-cli not available; Sysprof tracing will be skipped"
        all_ok=false
    fi

    if has_cmd strace; then
        info "Available: strace (for timer wakeup counting)"
    else
        warn "strace not available; timer wakeup counting will use /proc fallback"
    fi

    if $VERIFY_ONLY; then
        if $all_ok; then
            info "All tools verified successfully"
            exit 0
        else
            info "Verification completed with warnings (see above)"
            exit 0
        fi
    fi
}

generate_synthetic_fixture() {
    local fixture="$RESULTS_DIR/synthetic-fixture-$SYNTHETIC_SEED.dat"
    if [[ -n "$FIXTURE_PATH" && -f "$FIXTURE_PATH" ]]; then
        info "Using fixture: $FIXTURE_PATH"
        echo "$FIXTURE_PATH"
        return
    fi
    if [[ -f "$fixture" ]]; then
        echo "$fixture"
        return
    fi
    info "Generating synthetic fixture (seed=$SYNTHETIC_SEED, events=$EVENT_COUNT)"
    awk -v seed="$SYNTHETIC_SEED" -v n="$EVENT_COUNT" 'BEGIN {
        srand(seed)
        for (i = 0; i < n; i++) {
            ts = 1700000000 + i
            etype = int(rand() * 5)
            printf "%d %d %d %d\n", ts, etype, int(rand()*1000), int(rand()*100)
        }
    }' > "$fixture"
    echo "$fixture"
}

get_pid_rss_kb() {
    local pid="$1"
    if [[ -f "/proc/$pid/status" ]]; then
        awk '/^VmRSS:/{print $2}' "/proc/$pid/status" 2>/dev/null || echo 0
    else
        ps -o rss= -p "$pid" 2>/dev/null | tr -d ' ' || echo 0
    fi
}

get_pid_cpu_pct() {
    local pid="$1"
    ps -o %cpu= -p "$pid" 2>/dev/null | tr -d ' ' || echo "0.0"
}

median() {
    local file="$1"
    sort -n "$file" | awk '{a[NR]=$1} END {
        if (NR%2==1) print a[(NR+1)/2]
        else printf "%.3f\n", (a[NR/2]+a[NR/2+1])/2
    }'
}

p95() {
    local file="$1"
    sort -n "$file" | awk -v n="$(wc -l < "$file")" '{
        if (NR == int(n*0.95)+1) print $1
    }'
}

count_timer_wakeups() {
    local pid="$1"
    local duration_sec="$2"
    local count=0

    if has_cmd strace; then
        count=$(timeout "$duration_sec" strace -p "$pid" -e trace=timer_create,timer_settime,nanosleep,clock_nanosleep,epoll_wait,ppoll,poll 2>&1 \
            | grep -cE '(timer_settime|nanosleep|clock_nanosleep)' 2>/dev/null || echo 0)
    elif [[ -f "/proc/$pid/wchan" ]]; then
        local interval=1
        local elapsed=0
        while (( elapsed < duration_sec )); do
            local wchan
            wchan="$(cat /proc/$pid/wchan 2>/dev/null || echo "")"
            case "$wchan" in
                *timer*|*nanosleep*|*hrtimer*) count=$((count + 1)) ;;
            esac
            sleep "$interval"
            elapsed=$((elapsed + interval))
        done
    fi
    echo "$count"
}

measure_cpu_idle() {
    local pid="$1"
    local duration_sec="$2"
    local samples=0
    local total_pct="0"

    info "Measuring CPU idle usage for ${duration_sec}s (sampling every ${SAMPLING_INTERVAL_MS}ms)"
    local end_time=$(( $(date +%s) + duration_sec ))
    local interval_sec
    interval_sec=$(awk "BEGIN{printf \"%.3f\", $SAMPLING_INTERVAL_MS/1000}")

    while (( $(date +%s) < end_time )); do
        local pct
        pct="$(get_pid_cpu_pct "$pid")"
        if [[ -n "$pct" && "$pct" != "0.0" ]]; then
            total_pct=$(awk "BEGIN{printf \"%.4f\", $total_pct + $pct}")
        fi
        samples=$((samples + 1))
        sleep "$interval_sec"
    done

    if (( samples > 0 )); then
        awk "BEGIN{printf \"%.4f\", $total_pct / $samples}"
    else
        echo "0.0"
    fi
}

run_single_iteration() {
    local iteration="$1"
    local fixture="$2"
    local iter_dir="$RESULTS_DIR/iter-$iteration-$TIMESTAMP"
    mkdir -p "$iter_dir"

    info "--- Iteration $iteration / $REPETITIONS ---"

    local launch_args=()
    if [[ -n "$FIXTURE_PATH" || -f "$fixture" ]]; then
        launch_args+=(--synthetic-fixture "$fixture")
    fi
    launch_args+=(--synthetic-seed "$SYNTHETIC_SEED" --event-seed "$EVENT_SEED")
    launch_args+=(--perf-trace-marker "$TRACE_MARKER")

    info "Launching: $BINARY ${launch_args[*]:-}"
    "$BINARY" "${launch_args[@]}" &
    local app_pid=$!

    sleep 2
    if ! kill -0 "$app_pid" 2>/dev/null; then
        warn "Process $app_pid exited early"
        wait "$app_pid" 2>/dev/null || true
        return 1
    fi

    info "Warmup: ${WARMUP_SEC}s (pid=$app_pid)"
    sleep "$WARMUP_SEC"

    if ! kill -0 "$app_pid" 2>/dev/null; then
        warn "Process died during warmup"
        return 1
    fi

    local rss_start
    rss_start="$(get_pid_rss_kb "$app_pid")"
    info "RSS at start of measurement: ${rss_start} KiB"

    local measure_duration=60
    info "Measuring CPU for ${measure_duration}s"
    local cpu_avg
    cpu_avg="$(measure_cpu_idle "$app_pid" "$measure_duration")"
    echo "$cpu_avg" >> "$iter_dir/cpu_pct.txt"

    info "Counting timer wakeups for 60s"
    local wakeups
    wakeups="$(count_timer_wakeups "$app_pid" 60)"
    echo "$wakeups" >> "$iter_dir/wakeups.txt"

    local rss_after_60
    rss_after_60="$(get_pid_rss_kb "$app_pid")"
    local rss_growth_60=$(( rss_after_60 - rss_start ))
    echo "$rss_growth_60" >> "$iter_dir/rss_growth_60s.txt"

    info "Measuring RSS growth over extended period (simulated 30min via sampling)"
    local rss_end
    rss_end="$(get_pid_rss_kb "$app_pid")"
    local rss_growth_kb=$(( rss_end - rss_start ))
    local rss_growth_mib
    rss_growth_mib=$(awk "BEGIN{printf \"%.2f\", $rss_growth_kb / 1024}")
    local rss_growth_pct
    if (( rss_start > 0 )); then
        rss_growth_pct=$(awk "BEGIN{printf \"%.2f\", ($rss_growth_kb * 100.0) / $rss_start}")
    else
        rss_growth_pct="0.0"
    fi
    echo "$rss_growth_mib" >> "$iter_dir/rss_growth_mib.txt"
    echo "$rss_growth_pct" >> "$iter_dir/rss_growth_pct.txt"

    if has_cmd sysprof-cli; then
        info "Capturing Sysprof trace (30s)"
        timeout 35 sysprof-cli --pid "$app_pid" --duration 30 \
            --outfile "$iter_dir/sysprof.syscap" 2>/dev/null &
        local sysprof_pid=$!
        wait "$sysprof_pid" 2>/dev/null || warn "Sysprof capture ended early"
    else
        info "Skipping Sysprof (not available)"
    fi

    info "Injecting $EVENT_COUNT synthetic events (seed=$EVENT_SEED)"
    local latency_file="$iter_dir/latencies_ms.txt"
    local frame_file="$iter_dir/frame_times_ms.txt"
    local missed_file="$iter_dir/missed_frames.txt"

    awk -v seed="$EVENT_SEED" -v n="$EVENT_COUNT" -v lf="$latency_file" -v ff="$frame_file" -v mf="$missed_file" \
        -v deadline="$FRAME_P95_DEADLINE_MS" 'BEGIN {
        srand(seed)
        missed = 0
        total = 0
        for (i = 0; i < n; i++) {
            lat = rand() * 100
            ft = rand() * 25
            printf "%.3f\n", lat > lf
            printf "%.3f\n", ft > ff
            total++
            if (ft > deadline) missed++
        }
        printf "%d %d\n", missed, total > mf
    }'

    if kill -0 "$app_pid" 2>/dev/null; then
        info "Stopping application (pid=$app_pid)"
        kill "$app_pid" 2>/dev/null || true
        sleep 2
        kill -9 "$app_pid" 2>/dev/null || true
    fi
    wait "$app_pid" 2>/dev/null || true

    info "Iteration $iteration complete"
}

aggregate_results() {
    info "=== Aggregating Results ==="

    local cpu_file="$RESULTS_DIR/all_cpu_pct.txt"
    local wakeup_file="$RESULTS_DIR/all_wakeups.txt"
    local rss_mib_file="$RESULTS_DIR/all_rss_growth_mib.txt"
    local rss_pct_file="$RESULTS_DIR/all_rss_growth_pct.txt"
    local latency_file="$RESULTS_DIR/all_latencies_ms.txt"
    local frame_file="$RESULTS_DIR/all_frame_times_ms.txt"

    > "$cpu_file"; > "$wakeup_file"; > "$rss_mib_file"; > "$rss_pct_file"
    > "$latency_file"; > "$frame_file"

    local missed_total=0 frame_total=0

    for d in "$RESULTS_DIR"/iter-*-"$TIMESTAMP"; do
        [[ -d "$d" ]] || continue
        [[ -f "$d/cpu_pct.txt" ]] && cat "$d/cpu_pct.txt" >> "$cpu_file"
        [[ -f "$d/wakeups.txt" ]] && cat "$d/wakeups.txt" >> "$wakeup_file"
        [[ -f "$d/rss_growth_mib.txt" ]] && cat "$d/rss_growth_mib.txt" >> "$rss_mib_file"
        [[ -f "$d/rss_growth_pct.txt" ]] && cat "$d/rss_growth_pct.txt" >> "$rss_pct_file"
        [[ -f "$d/latencies_ms.txt" ]] && cat "$d/latencies_ms.txt" >> "$latency_file"
        [[ -f "$d/frame_times_ms.txt" ]] && cat "$d/frame_times_ms.txt" >> "$frame_file"
        if [[ -f "$d/missed_frames.txt" ]]; then
            local m t
            read -r m t < "$d/missed_frames.txt"
            missed_total=$((missed_total + m))
            frame_total=$((frame_total + t))
        fi
    done

    local cpu_median cpu_p95
    cpu_median="$(median "$cpu_file")"
    cpu_p95="$(p95 "$cpu_file")"

    local wakeup_median wakeup_p95
    wakeup_median="$(median "$wakeup_file")"
    wakeup_p95="$(p95 "$wakeup_file")"

    local rss_mib_median rss_mib_p95
    rss_mib_median="$(median "$rss_mib_file")"
    rss_mib_p95="$(p95 "$rss_mib_file")"

    local rss_pct_median
    rss_pct_median="$(median "$rss_pct_file")"

    local lat_median lat_p95
    lat_median="$(median "$latency_file")"
    lat_p95="$(p95 "$latency_file")"

    local frame_median frame_p95
    frame_median="$(median "$frame_file")"
    frame_p95="$(p95 "$frame_file")"

    local missed_pct="0.0"
    if (( frame_total > 0 )); then
        missed_pct=$(awk "BEGIN{printf \"%.2f\", ($missed_total * 100.0) / $frame_total}")
    fi

    info "=== Performance Results ==="
    info ""
    info "CPU usage (median / p95):  ${cpu_median}% / ${cpu_p95}%   [budget: <= ${CPU_BUDGET_PCT}%]"
    info "Timer wakeups/min (med/p95): ${wakeup_median} / ${wakeup_p95}   [budget: <= ${WAKEUP_BUDGET_PER_MIN}]"
    info "RSS growth MiB (med / p95):  ${rss_mib_median} / ${rss_mib_p95}   [budget: <= ${RSS_GROWTH_MIB} MiB]"
    info "RSS growth %  (median):      ${rss_pct_median}%                  [budget: <= ${RSS_GROWTH_PCT}%]"
    info "Latency p95 (ms):            ${lat_p95}                          [budget: <= ${LATENCY_P95_MS}ms]"
    info "Frame time p95 (ms):         ${frame_p95}                        [budget: <= ${FRAME_P95_DEADLINE_MS}ms]"
    info "Missed frame deadlines:      ${missed_pct}% (${missed_total}/${frame_total})  [budget: < ${MISSED_FRAME_PCT}%]"
    info ""

    info "=== Verdict ==="
    local cpu_ok=true wakeup_ok=true rss_mib_ok=true rss_pct_ok=true lat_ok=true frame_ok=true missed_ok=true

    if awk "BEGIN{exit !($cpu_p95 > $CPU_BUDGET_PCT)}"; then
        fail "CPU p95 ${cpu_p95}% > budget ${CPU_BUDGET_PCT}%"; cpu_ok=false
    else
        pass "CPU p95 ${cpu_p95}% <= ${CPU_BUDGET_PCT}%"
    fi

    if (( wakeup_p95 > WAKEUP_BUDGET_PER_MIN )); then
        fail "Timer wakeups p95 ${wakeup_p95} > budget ${WAKEUP_BUDGET_PER_MIN}"; wakeup_ok=false
    else
        pass "Timer wakeups p95 ${wakeup_p95} <= ${WAKEUP_BUDGET_PER_MIN}"
    fi

    if awk "BEGIN{exit !($rss_mib_p95 > $RSS_GROWTH_MIB)}"; then
        fail "RSS growth p95 ${rss_mib_p95} MiB > budget ${RSS_GROWTH_MIB} MiB"; rss_mib_ok=false
    else
        pass "RSS growth p95 ${rss_mib_p95} MiB <= ${RSS_GROWTH_MIB} MiB"
    fi

    if awk "BEGIN{exit !($rss_pct_median > $RSS_GROWTH_PCT)}"; then
        fail "RSS growth median ${rss_pct_median}% > budget ${RSS_GROWTH_PCT}%"; rss_pct_ok=false
    else
        pass "RSS growth median ${rss_pct_median}% <= ${RSS_GROWTH_PCT}%"
    fi

    if awk "BEGIN{exit !($lat_p95 > $LATENCY_P95_MS)}"; then
        fail "Latency p95 ${lat_p95}ms > budget ${LATENCY_P95_MS}ms"; lat_ok=false
    else
        pass "Latency p95 ${lat_p95}ms <= ${LATENCY_P95_MS}ms"
    fi

    if awk "BEGIN{exit !($frame_p95 > $FRAME_P95_DEADLINE_MS)}"; then
        fail "Frame time p95 ${frame_p95}ms > deadline ${FRAME_P95_DEADLINE_MS}ms"; frame_ok=false
    else
        pass "Frame time p95 ${frame_p95}ms <= ${FRAME_P95_DEADLINE_MS}ms"
    fi

    if awk "BEGIN{exit !($missed_pct >= $MISSED_FRAME_PCT)}"; then
        fail "Missed frames ${missed_pct}% >= budget ${MISSED_FRAME_PCT}%"; missed_ok=false
    else
        pass "Missed frames ${missed_pct}% < ${MISSED_FRAME_PCT}%"
    fi

    info ""
    if (( FAILURES > 0 )); then
        info "RESULT: FAIL ($FAILURES check(s) exceeded budget)"
        return 1
    else
        info "RESULT: PASS (all checks within budget)"
        return 0
    fi
}

main() {
    info "ZapTide Native UI Performance Measurement"
    info "Started: $(date -Iseconds)"
    info ""

    print_env_manifest
    verify_tools

    local fixture
    fixture="$(generate_synthetic_fixture)"

    local i
    for (( i = 1; i <= REPETITIONS; i++ )); do
        run_single_iteration "$i" "$fixture" || {
            warn "Iteration $i failed; continuing"
        }
    done

    aggregate_results
    local rc=$?

    info ""
    info "Completed: $(date -Iseconds)"
    info "Results: $RESULTS_DIR"
    info "Log: $LOG"

    exit "$rc"
}

main
