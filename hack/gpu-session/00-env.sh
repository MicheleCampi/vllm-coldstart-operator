#!/usr/bin/env bash
# Item-4 GPU session parameters. Fill the CHANGE-ME values at launch, then
# `source hack/gpu-session/00-env.sh` before every other script.
# Topology: A=serving, B=serving (preempted mid-run), C=warm spare.

# --- fill at launch (Lambda dashboard) ---
export NODE_A_IP="CHANGE-ME"          # public IP, k3s server
export NODE_B_IP="CHANGE-ME"          # public IP, agent
export NODE_C_IP="CHANGE-ME"          # public IP, agent
export SSH_KEY="${SSH_KEY:-$HOME/.ssh/lambda}"   # key used for ubuntu@<ip>

# --- pinned versions (decided 2026-07-04, do not drift mid-session) ---
export K3S_VERSION="v1.36.2+k3s1"
export DEVICE_PLUGIN_VERSION="v0.19.3"
export VLLM_IMAGE="vllm/vllm-openai@sha256:6d8429e38e3747723ca07ee1b17972e09bb9c51c4032b266f24fb1cc3b22ed8f"  # v0.23.0
export MODEL="Qwen/Qwen2.5-7B-Instruct"
export HF_CACHE_DIR="/opt/hf-cache"

ssh_a() { ssh -i "$SSH_KEY" -o StrictHostKeyChecking=accept-new "ubuntu@$NODE_A_IP" "$@"; }
ssh_b() { ssh -i "$SSH_KEY" -o StrictHostKeyChecking=accept-new "ubuntu@$NODE_B_IP" "$@"; }
ssh_c() { ssh -i "$SSH_KEY" -o StrictHostKeyChecking=accept-new "ubuntu@$NODE_C_IP" "$@"; }
