"""Transcribe one audio file with a local Whisper model (faster-whisper). Prints the text.

ffmpeg decodes any browser format (webm/opus, ogg, m4a, wav) to 16 kHz mono samples, which go
to the model as an array, so no Python audio decoder is needed.
"""
import subprocess
import sys

import numpy as np
from faster_whisper import WhisperModel

pcm = subprocess.run(
    ["ffmpeg", "-nostdin", "-loglevel", "error", "-i", sys.argv[1], "-ar", "16000", "-ac", "1", "-f", "s16le", "-"],
    check=True,
    capture_output=True,
).stdout
audio = np.frombuffer(pcm, dtype=np.int16).astype(np.float32) / 32768.0
model = WhisperModel("tiny.en", device="cpu", compute_type="int8", download_root="/models")
segments, _info = model.transcribe(audio, beam_size=1, vad_filter=True)
print(" ".join(s.text.strip() for s in segments).strip())
