# Browser test layer (front-end use case tests, spec §15.1): adds a headless Chromium driven
# from shell scripts. Heavy, so it is built only when a project defines a browser test type
# (`make images-browser`).
FROM kodehub/test-python:dev
USER root
RUN pip install --no-cache-dir playwright==1.55.0 \
 && playwright install --with-deps chromium \
 && rm -rf /var/lib/apt/lists/*
USER kodehub
LABEL kodehub.test-container-type="browser" kodehub.layers="core,python,browser"
