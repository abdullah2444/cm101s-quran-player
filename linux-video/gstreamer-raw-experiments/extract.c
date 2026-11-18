/* Extract a short H.264 Annex B sample for the isolated hardware sink test. */
#include <stdio.h>
#include <stdlib.h>
#include <libavformat/avformat.h>
#include <libavcodec/avcodec.h>

int main(int argc, char **argv) {
    AVFormatContext *fmt = NULL;
    AVBSFContext *bsf = NULL;
    AVPacket *in = av_packet_alloc(), *out = av_packet_alloc();
    FILE *dest = NULL;
    int stream, count = 0, rc = 1;
    if ((argc != 3 && argc != 4) || !in || !out) goto done;
    if (avformat_open_input(&fmt, argv[1], NULL, NULL) < 0 || avformat_find_stream_info(fmt, NULL) < 0) goto done;
    stream = av_find_best_stream(fmt, AVMEDIA_TYPE_VIDEO, -1, -1, NULL, 0);
    if (stream < 0 || fmt->streams[stream]->codecpar->codec_id != AV_CODEC_ID_H264) goto done;
    fprintf(stderr, "Source video %dx%d\n", fmt->streams[stream]->codecpar->width, fmt->streams[stream]->codecpar->height);
    if (av_bsf_alloc(av_bsf_get_by_name("h264_mp4toannexb"), &bsf) < 0) goto done;
    if (avcodec_parameters_copy(bsf->par_in, fmt->streams[stream]->codecpar) < 0) goto done;
    bsf->time_base_in = fmt->streams[stream]->time_base;
    if (av_bsf_init(bsf) < 0 || !(dest = fopen(argv[2], "wb"))) goto done;
    int limit = argc == 4 ? atoi(argv[3]) : 125;
    if (limit < 1 || limit > 125) goto done;
    while (count < limit && av_read_frame(fmt, in) >= 0) {
        if (in->stream_index == stream && av_bsf_send_packet(bsf, in) >= 0) {
            while (av_bsf_receive_packet(bsf, out) >= 0) {
                if (fwrite(out->data, 1, out->size, dest) != (size_t)out->size) goto done;
                ++count; av_packet_unref(out);
            }
        }
        av_packet_unref(in);
    }
    if (count) { fprintf(stderr, "Extracted %d encoded packets\n", count); rc = 0; }
done:
    if (dest && fclose(dest)) rc = 1;
    av_bsf_free(&bsf); av_packet_free(&in); av_packet_free(&out);
    if (fmt) avformat_close_input(&fmt);
    return rc;
}
