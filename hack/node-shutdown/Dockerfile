# node-shutdown job (ADR-0028): `vllm` on PATH is fake-vllm.py, so a serving
# VllmService (cpu and memory set) runs it with the operator's real command,
# probes and pre-stop hook. client.py is the load in the same image.
FROM public.ecr.aws/docker/library/python:3.14.10-slim-trixie@sha256:9aaa474097d094040649f2162a627c0049b408f52138d980916788d80f04802f
COPY fake-vllm.py /usr/local/bin/vllm
COPY client.py /client.py
RUN chmod 0755 /usr/local/bin/vllm
