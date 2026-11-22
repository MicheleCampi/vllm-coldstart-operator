#!/bin/bash
# EC2 user-data for the Sapphire Rapids bisect (see bisect.sh). Amazon Linux
# 2023 on m7i.large, launched with instance-initiated-shutdown-behavior
# terminate: whatever happens, the last line powers the instance off and AWS
# terminates it. No SSH, no inbound port; results are read with
#   aws ec2 get-console-output --latest --instance-id ID --output text | grep BISECT
# COMMIT is replaced at launch with the commit of this branch to check out.
# A hard stop at 170 min keeps the session inside its 3 h budget.
exec > >(tee -a /var/log/bisect.log /dev/console) 2>&1
( sleep 10200; echo "BISECT TIMEOUT 170 min, powering off"; shutdown -h now ) &
set -x
dnf install -y -q docker git python3 > /dev/null
systemctl start docker
git clone -q https://github.com/MicheleCampi/vllm-coldstart-operator /opt/op
cd /opt/op && git checkout -q COMMIT
export MODEL=Qwen/Qwen2.5-0.5B-Instruct MODEL_REVISION=7ae557604adf67be50417f59c2c2f167def9a775 HF_CACHE=/opt/hf REPS=2 OUT=/opt/out
set +x
first=$(grep -v '^#' hack/sapphire-bisect/images.txt | head -1 | awk '{print $2}')
bash hack/first-request/fetch-model.sh "$first" || echo "BISECT model fetch FAILED"
bash hack/sapphire-bisect/bisect.sh || echo "BISECT bisect.sh exited $?"
echo "BISECT powering off at $(date -u +%H:%M:%SZ)"
sync; shutdown -h now
