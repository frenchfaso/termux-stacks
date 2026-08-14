FROM alpine:3.24.1
COPY probe /probe
WORKDIR /fixture-wd
ENV TSTACK_S1_IMAGE=image-default
ENTRYPOINT []
CMD ["/probe", "C1", "C two"]
