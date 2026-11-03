# a-scorer: batch scoring of admitted A artifacts loaded by digest (no model in the image).
# Build context: container/build/ (ignored by Git), assembled by the operator with
#   ort/libonnxruntime.so.1.31.1  (onnxruntime 1.31.1 cp312 manylinux_2_28_x86_64 wheel,
#                                  sha256 fc44112fbe06d7d689c57e11d3862031f64711007e4ee020f8eed5185fd2c1d3)
#   bin/a-scorer                  (built in rust:1.99.1-slim-bookworm, pinned by digest in the served profile)
# Runtime base: gcr.io/distroless/cc-debian12:nonroot (glibc 2.36, libstdc++ 12), pinned by digest.
FROM gcr.io/distroless/cc-debian12@sha256:9dac0a79194e45a7da0158a9c6da57b217585af0786db3845d1f0ec1a0dd182f
COPY --chmod=0555 ort/libonnxruntime.so.1.31.1 /opt/ort/libonnxruntime.so.1.31.1
COPY --chmod=0555 bin/a-scorer /usr/local/bin/a-scorer
USER 65532:65532
ENV A_WORK_DIR=/tmp/a-scorer A_SESSIONS=4 A_THREADS=1
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/a-scorer"]
