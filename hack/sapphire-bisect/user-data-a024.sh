#!/bin/bash
# EC2 user-data for session A-024 (see a024.sh). Amazon Linux 2023 on
# m7i-flex.large, launched with instance-initiated-shutdown-behavior
# terminate: whatever happens, the last line powers the instance off and AWS
# terminates it. No SSH, no inbound port; results are read with
#   aws ec2 get-console-output --latest --instance-id ID --output text | grep A024
# COMMIT is replaced at launch with the commit of this branch to check out.
# A hard stop at 80 min keeps the session inside its budget.
exec > >(tee -a /var/log/a024.log /dev/console) 2>&1
( sleep 4800; echo "A024 TIMEOUT 80 min, powering off"; shutdown -h now ) &
set -x
dnf install -y -q docker git python3 > /dev/null
systemctl start docker
git clone -q https://github.com/MicheleCampi/vllm-coldstart-operator /opt/op
cd /opt/op && git checkout -q COMMIT
export MODEL=Qwen/Qwen2.5-0.5B-Instruct MODEL_REVISION=7ae557604adf67be50417f59c2c2f167def9a775 HF_CACHE=/opt/hf REPS=2 OUT=/opt/out
set +x
bash hack/first-request/fetch-model.sh docker.io/vllm/vllm-openai-cpu:v0.19.1-x86_64@sha256:4c697ae650ebeb3a41f3c9c7020913d4c84d2729dc428ce39d60ca353975a4ce || echo "A024 model fetch FAILED"
bash hack/sapphire-bisect/a024.sh || echo "A024 a024.sh exited $?"
echo "A024 powering off at $(date -u +%H:%M:%SZ)"
sync; shutdown -h now
