#!/bin/sh
# watch.sh <pid> [1]: remote GPU uses free > 40%; heavy local work also needs load < 20.
# Pass 1 when handing over a job that is already stopped.
pid=$1 stopped=${2:-0}  # pass 1 when handing over a job that is already stopped
local_model=0
command=$(ps -p "$pid" -o command=)
if [ -n "${WATCH_SOFT:-}" ]; then
  case "$command" in
    *'--system hyperag '*)
      profile=$(echo "$command" | awk '{for(i=1;i<NF;i++) if($i=="--profile") print $(i+1)}')
      case "$profile" in e5*|egemma2*|bge*|*+rerank*|*+bgev2m3*) local_model=1 ;; esac ;;
  esac
fi
local_step=$local_model
case "$command" in *fastembed_server.py*|*hindsight-api*) local_step=1 ;; esac
load_wait_started=${3:-0} load_wait_reported=0
while kill -0 $pid 2>/dev/null; do
  free=$(memory_pressure | awk '/free percentage/ {print $5+0}')
  swap=$(sysctl -n vm.swapusage | awk '{v=$6; sub(/M/,"",v); print int(v)}')
  load=$(sysctl -n vm.loadavg | awk '{print int($2)}')
  if [ -n "${WATCH_SOFT:-}" ]; then
    # Chief: remote GPU uses only free > 40%; local model work keeps the full gate.
    # Preserve the eight calm samples before resuming.
    load=$(sysctl -n vm.loadavg | awk '{print $2+0}')
    state=$(awk -v f="$free" -v l="$load" -v local_model="$local_model" 'BEGIN {if (f <= 40 || (local_model && l >= 20)) print "bad"; else print "calm"}')
    case "$state" in
      bad) bad=1; calm=0 ;;
      calm) bad=""; calm=$((${calm:-0} + 1)) ;;
      *) bad=""; calm=0 ;;
    esac
    if [ "${calm:-0}" -ge 8 ]; then good=1; else good=""; fi
  else
    load=$(sysctl -n vm.loadavg | awk '{print $2+0}')
    bad=$(awk -v f="$free" -v l="$load" 'BEGIN {if (f <= 40 || l >= 20) print 1}')
    good=$(awk -v f="$free" -v l="$load" 'BEGIN {if (f > 40 && l < 20) print 1}')
  fi
  if [ -n "$bad" ]; then
    [ $stopped = 0 ] && { kill -STOP $pid; pkill -STOP -P $pid; stopped=1; echo "$(date -u +%H:%MZ) paused (free $free%, swap ${swap}M, load $load)"; }
  elif [ -n "$good" ] && [ $stopped = 1 ]; then
    kill -CONT $pid; pkill -CONT -P $pid; stopped=0; echo "$(date -u +%H:%MZ) resumed (free $free%, swap ${swap}M, load $load)"
  fi
  if [ "$local_step" = 1 ] && [ "$stopped" = 1 ] && awk -v f="$free" -v l="$load" 'BEGIN {exit !(f<=40 || l>=20)}'; then
    now=$(date +%s)
    [ "$load_wait_started" = 0 ] && load_wait_started=$now
    if [ $((now - load_wait_started)) -gt 600 ] && [ "$load_wait_reported" = 0 ] && [ "$free" -gt 40 ]; then
      echo "LOCAL GATE WAIT >10m pid=$pid: preserve partial evidence and swap behind the next memory-eligible GPU-only row"
      load_wait_reported=1
    fi
  else
    load_wait_started=0 load_wait_reported=0
  fi
  sleep 15
done
