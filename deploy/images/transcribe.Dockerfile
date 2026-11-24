# Speech transcription container (spec §14.1): a local Whisper model (faster-whisper, tiny.en,
# baked in at build time) — audio never leaves the cluster and no browser speech service is used.
FROM kodehub/test-core:dev
USER root
RUN apt-get update && apt-get install -y --no-install-recommends ffmpeg && rm -rf /var/lib/apt/lists/* \
 && pip install --no-cache-dir faster-whisper==1.2.0 requests==2.32.5 \
 && mkdir -p /models \
 && python -c "from faster_whisper import WhisperModel; WhisperModel('tiny.en', device='cpu', compute_type='int8', download_root='/models')" \
 && chown -R kodehub /models
COPY transcribe/transcribe.py /app/transcribe.py
USER kodehub
ENV KODEHUB_TRANSCRIBE_CMD="python3 /app/transcribe.py"
