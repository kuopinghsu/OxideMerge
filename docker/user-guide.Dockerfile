FROM ubuntu:24.04

# Match the Asciidoctor and font dependencies of the CI user-guide job.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl perl ruby \
        fontconfig fonts-dejavu-core python3-fonttools \
    && gem install --no-document asciidoctor:2.0.26 asciidoctor-pdf:2.3.15 \
    && rm -rf /var/lib/apt/lists/*
