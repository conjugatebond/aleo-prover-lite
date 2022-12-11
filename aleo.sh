#!/bin/bash
set -o pipefail

WORKSPACE=$PWD
LOG_PATH="$WORKSPACE/prover.log"
APP_PATH="$WORKSPACE/aleo-prover"

worker=aleo1ngnuq34atp4ev5m3gx3h7h0ps3wgz4p437lsrq0w9u8pfcva759qflt25n

cpu_cores=$(lscpu | grep '^CPU(s):' | awk '{print $2}')
cpu_affinity=($(nvidia-smi topo -m 2>/dev/null | awk -F'\t+| {2,}' '{for (i=1;i<=NF;i++) if($i ~ /CPU Affinity/) col=i; if (NR != 1 && $0 ~ /^GPU/) print $col}'))
gpu_num=${#cpu_affinity[*]}

cat <<EOF
======================================


Number of gpus: $gpu_num
Number of cores: $cpu_cores

======================================

EOF

if [ $gpu_num -eq 0 ]; then
    nohup $APP_PATH -a $worker >> $LOG_PATH 2>&1 &

    echo "nohup $APP_PATH -a \"$worker\" >> $LOG_PATH 2>&1 &"
elif [ $gpu_num -eq 1 ]; then
    nohup $APP_PATH -g 0 -a $worker >> $LOG_PATH 2>&1 &
    echo "nohup $APP_PATH -g 0 -a \"$worker\" >> $LOG_PATH 2>&1 &"
else
    median=$(( cpu_cores / 2))
    span=$(( median / gpu_num ))

    for gpu_seq in $(seq 0 $((gpu_num-1))); do
        cpu_list="$((gpu_seq * span))-$(((gpu_seq+1) * span - 1)),$((gpu_seq * span + median))-$(((gpu_seq+1) * span + median - 1))"
        export CUDA_VISIBLE_DEVICES=$gpu_seq
        nohup taskset -c $cpu_list $APP_PATH -g $gpu_seq -a $worker >> $LOG_PATH 2>&1 &

        echo "nohup taskset -c $cpu_list $APP_PATH -g $gpu_seq  -a \"$worker\" >> $LOG_PATH 2>&1 &"
    done
fi
