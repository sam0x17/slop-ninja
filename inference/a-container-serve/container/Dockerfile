# a-container-serve: f32 banded ONNX serving of the A release candidate.
# The build context is assembled outside Git (see ../README.md): ort/, model/ and bin/.
# Runtime base: gcr.io/distroless/cc-debian12:nonroot (glibc 2.36, libstdc++ 12), pinned by digest.
FROM gcr.io/distroless/cc-debian12@sha256:9dac0a79194e45a7da0158a9c6da57b217585af0786db3845d1f0ec1a0dd182f
# ONNX Runtime 1.31.1 CPU library from the onnxruntime 1.31.1 cp312 manylinux_2_28_x86_64 wheel
# (library sha256 fc44112fbe06d7d689c57e11d3862031f64711007e4ee020f8eed5185fd2c1d3).
COPY --chmod=0555 ort/libonnxruntime.so.1.31.1 /opt/ort/libonnxruntime.so.1.31.1
# Served files of profiles/a-baseline-rc-s17-f32band.json; the server checks each digest.
COPY --chmod=0555 model/a-fp32band.onnx /model/a-fp32band.onnx
COPY --chmod=0555 model/heads.json model/contract.json model/calibration.json model/manifest.json model/tokenizer.json /model/
COPY --chmod=0555 bin/a-container-serve /usr/local/bin/a-container-serve
# Directories and files are read-only (0555: directories need search permission).
USER 65532:65532
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/a-container-serve"]
