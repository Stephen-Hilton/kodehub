# Python test layer: the core layer's interpreter; bundles vendor pytest and dependencies.
FROM kodehub/test-core:dev
LABEL kodehub.test-container-type="python" kodehub.layers="core,python"
