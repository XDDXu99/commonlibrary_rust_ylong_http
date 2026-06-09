/*
 * Local libcurl multi benchmark for HTTP or HTTPS over HTTPS proxy.
 *
 * The program is intentionally standalone so the benchmark does not add Rust
 * crate dependencies. It expects the local target/proxy servers to be started
 * by scripts/bench_https_proxy.sh.
 */

#include <curl/curl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

typedef struct {
    const char *target_url;
    const char *proxy_url;
    const char *proxy_ca;
    const char *target_ca;
    const char *resolve;
    long requests;
    long concurrency;
    long response_size;
    int keep_alive;
    int proxy_insecure;
} Config;

typedef struct {
    CURL *easy;
    long worker;
    long remaining;
    long completed;
    long errors;
    size_t current_bytes;
    unsigned long long started_ns;
} Slot;

typedef struct {
    double *latencies;
    long latency_count;
    double first_sum;
    long first_count;
    double steady_sum;
    long steady_count;
    long total_errors;
    size_t body_bytes;
} Stats;

static unsigned long long now_ns(void)
{
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (unsigned long long)ts.tv_sec * 1000000000ULL + (unsigned long long)ts.tv_nsec;
}

static void usage(const char *program)
{
    fprintf(stderr,
        "Usage: %s --target-url URL --proxy-url URL --proxy-ca PEM "
        "[--target-ca PEM] "
        "--requests N --concurrency N [--response-size N] "
        "[--resolve HOST:PORT:ADDR] [--keep-alive|--cold] [--proxy-insecure]\n",
        program);
}

static const char *arg_value(int argc, char **argv, const char *name, const char *fallback)
{
    for (int i = 1; i + 1 < argc; i++) {
        if (strcmp(argv[i], name) == 0) {
            return argv[i + 1];
        }
    }
    return fallback;
}

static int has_flag(int argc, char **argv, const char *name)
{
    for (int i = 1; i < argc; i++) {
        if (strcmp(argv[i], name) == 0) {
            return 1;
        }
    }
    return 0;
}

static long parse_positive_long(const char *value, const char *name)
{
    char *end = NULL;
    long parsed = strtol(value, &end, 10);
    if (end == value || *end != '\0' || parsed <= 0) {
        fprintf(stderr, "%s must be a positive integer\n", name);
        return -1;
    }
    return parsed;
}

static long worker_request_count(long requests, long concurrency, long worker)
{
    long base = requests / concurrency;
    long remainder = requests % concurrency;
    return base + (worker < remainder ? 1 : 0);
}

static size_t write_body(char *ptr, size_t size, size_t nmemb, void *userdata)
{
    (void)ptr;
    Slot *slot = (Slot *)userdata;
    size_t bytes = size * nmemb;
    slot->current_bytes += bytes;
    return bytes;
}

static int double_compare(const void *left, const void *right)
{
    double a = *(const double *)left;
    double b = *(const double *)right;
    if (a < b) {
        return -1;
    }
    if (a > b) {
        return 1;
    }
    return 0;
}

static double percentile(double *values, long count, double fraction)
{
    if (count <= 0) {
        return 0.0;
    }
    long index = (long)(count * fraction);
    if ((double)index < (double)count * fraction) {
        index++;
    }
    if (index < 1) {
        index = 1;
    }
    if (index > count) {
        index = count;
    }
    return values[index - 1];
}

static int configure_easy(CURLM *multi, Slot *slot, const Config *config, struct curl_slist *resolve)
{
    if (slot->easy == NULL) {
        slot->easy = curl_easy_init();
        if (slot->easy == NULL) {
            fprintf(stderr, "curl_easy_init failed\n");
            return 1;
        }
    } else {
        curl_easy_reset(slot->easy);
    }

    curl_easy_setopt(slot->easy, CURLOPT_URL, config->target_url);
    curl_easy_setopt(slot->easy, CURLOPT_PROXY, config->proxy_url);
    curl_easy_setopt(slot->easy, CURLOPT_HTTP_VERSION, CURL_HTTP_VERSION_1_1);
    curl_easy_setopt(slot->easy, CURLOPT_WRITEFUNCTION, write_body);
    curl_easy_setopt(slot->easy, CURLOPT_WRITEDATA, slot);
    curl_easy_setopt(slot->easy, CURLOPT_PRIVATE, slot);
    curl_easy_setopt(slot->easy, CURLOPT_NOSIGNAL, 1L);
    curl_easy_setopt(slot->easy, CURLOPT_FAILONERROR, 1L);
    curl_easy_setopt(slot->easy, CURLOPT_PROXY_SSL_VERIFYPEER, config->proxy_insecure ? 0L : 1L);
    curl_easy_setopt(slot->easy, CURLOPT_PROXY_SSL_VERIFYHOST, config->proxy_insecure ? 0L : 2L);
    if (config->proxy_ca != NULL && config->proxy_ca[0] != '\0') {
        curl_easy_setopt(slot->easy, CURLOPT_PROXY_CAINFO, config->proxy_ca);
    }
    if (config->target_ca != NULL && config->target_ca[0] != '\0') {
        curl_easy_setopt(slot->easy, CURLOPT_CAINFO, config->target_ca);
    }
    if (resolve != NULL) {
        curl_easy_setopt(slot->easy, CURLOPT_RESOLVE, resolve);
    }
    if (!config->keep_alive) {
        curl_easy_setopt(slot->easy, CURLOPT_FRESH_CONNECT, 1L);
        curl_easy_setopt(slot->easy, CURLOPT_FORBID_REUSE, 1L);
    }

    slot->current_bytes = 0;
    slot->started_ns = now_ns();
    CURLMcode code = curl_multi_add_handle(multi, slot->easy);
    if (code != CURLM_OK) {
        fprintf(stderr, "curl_multi_add_handle failed: %s\n", curl_multi_strerror(code));
        return 1;
    }
    return 0;
}

static void record_done(Slot *slot, CURLcode result, Stats *stats)
{
    unsigned long long elapsed_ns = now_ns() - slot->started_ns;
    double elapsed_ms = (double)elapsed_ns / 1000000.0;
    long response_code = 0;
    curl_easy_getinfo(slot->easy, CURLINFO_RESPONSE_CODE, &response_code);

    stats->latencies[stats->latency_count++] = elapsed_ms;
    if (slot->completed == 0) {
        stats->first_sum += elapsed_ms;
        stats->first_count++;
    } else {
        stats->steady_sum += elapsed_ms;
        stats->steady_count++;
    }

    if (result != CURLE_OK || response_code != 200) {
        slot->errors++;
        stats->total_errors++;
    }
    stats->body_bytes += slot->current_bytes;
    slot->completed++;
    slot->remaining--;
}

static void print_stats(const Config *config, Stats *stats, Slot *slots, double total_ms)
{
    qsort(stats->latencies, (size_t)stats->latency_count, sizeof(double), double_compare);

    double sum = 0.0;
    for (long i = 0; i < stats->latency_count; i++) {
        sum += stats->latencies[i];
    }

    double avg = stats->latency_count == 0 ? 0.0 : sum / (double)stats->latency_count;
    double p95 = percentile(stats->latencies, stats->latency_count, 0.95);
    double p99 = percentile(stats->latencies, stats->latency_count, 0.99);
    double min = stats->latency_count == 0 ? 0.0 : stats->latencies[0];
    double max = stats->latency_count == 0 ? 0.0 : stats->latencies[stats->latency_count - 1];
    double first_avg = stats->first_count == 0 ? 0.0 : stats->first_sum / (double)stats->first_count;
    double steady_avg = stats->steady_count == 0 ? 0.0 : stats->steady_sum / (double)stats->steady_count;
    double rps = total_ms <= 0.0 ? 0.0 : (double)config->requests * 1000.0 / total_ms;
    long per_worker_min = config->requests / config->concurrency;
    long per_worker_max = per_worker_min + (config->requests % config->concurrency != 0 ? 1 : 0);

    char *per_worker_errors = calloc((size_t)config->concurrency * 24 + 1, 1);
    if (per_worker_errors == NULL) {
        per_worker_errors = "";
    } else {
        size_t used = 0;
        for (long i = 0; i < config->concurrency; i++) {
            int written = snprintf(
                per_worker_errors + used,
                (size_t)config->concurrency * 24 + 1 - used,
                "%s%ld",
                i == 0 ? "" : ",",
                slots[i].errors);
            if (written < 0) {
                break;
            }
            used += (size_t)written;
        }
    }

    printf(
        "client=libcurl mode=%s requests=%ld concurrency=%ld total_ms=%.3f rps=%.3f "
        "avg_ms=%.3f p95_ms=%.3f p99_ms=%.3f first_request_ms=%.3f "
        "steady_avg_ms=%.3f min_ms=%.3f max_ms=%.3f body_bytes=%zu "
        "response_size=%ld workers=%ld requests_per_worker_min=%ld "
        "requests_per_worker_max=%ld total_errors=%ld per_worker_errors=%s errors=%ld\n",
        config->keep_alive ? "keep-alive" : "cold",
        config->requests,
        config->concurrency,
        total_ms,
        rps,
        avg,
        p95,
        p99,
        first_avg,
        steady_avg,
        min,
        max,
        stats->body_bytes,
        config->response_size,
        config->concurrency,
        per_worker_min,
        per_worker_max,
        stats->total_errors,
        per_worker_errors,
        stats->total_errors);

    if (per_worker_errors[0] != '\0') {
        free(per_worker_errors);
    }
}

int main(int argc, char **argv)
{
    Config config;
    memset(&config, 0, sizeof(config));
    config.target_url = arg_value(argc, argv, "--target-url", NULL);
    config.proxy_url = arg_value(argc, argv, "--proxy-url", NULL);
    config.proxy_ca = arg_value(argc, argv, "--proxy-ca", "");
    config.target_ca = arg_value(argc, argv, "--target-ca", "");
    config.resolve = arg_value(argc, argv, "--resolve", NULL);
    config.requests = parse_positive_long(arg_value(argc, argv, "--requests", "0"), "requests");
    config.concurrency = parse_positive_long(arg_value(argc, argv, "--concurrency", "0"), "concurrency");
    config.response_size = parse_positive_long(arg_value(argc, argv, "--response-size", "1024"), "response-size");
    config.keep_alive = !has_flag(argc, argv, "--cold");
    config.proxy_insecure = has_flag(argc, argv, "--proxy-insecure");

    if (config.target_url == NULL || config.proxy_url == NULL ||
        config.requests <= 0 || config.concurrency <= 0 || config.response_size <= 0) {
        usage(argv[0]);
        return 2;
    }

    if (curl_global_init(CURL_GLOBAL_DEFAULT) != 0) {
        fprintf(stderr, "curl_global_init failed\n");
        return 1;
    }

    CURLM *multi = curl_multi_init();
    if (multi == NULL) {
        fprintf(stderr, "curl_multi_init failed\n");
        curl_global_cleanup();
        return 1;
    }
    curl_multi_setopt(multi, CURLMOPT_MAX_TOTAL_CONNECTIONS, config.concurrency);
    curl_multi_setopt(multi, CURLMOPT_MAX_HOST_CONNECTIONS, config.concurrency);

    struct curl_slist *resolve = NULL;
    if (config.resolve != NULL && config.resolve[0] != '\0') {
        resolve = curl_slist_append(resolve, config.resolve);
    }

    Slot *slots = calloc((size_t)config.concurrency, sizeof(Slot));
    Stats stats;
    memset(&stats, 0, sizeof(stats));
    stats.latencies = calloc((size_t)config.requests, sizeof(double));
    if (slots == NULL || stats.latencies == NULL) {
        fprintf(stderr, "allocation failed\n");
        free(slots);
        free(stats.latencies);
        curl_slist_free_all(resolve);
        curl_multi_cleanup(multi);
        curl_global_cleanup();
        return 1;
    }

    unsigned long long total_started = now_ns();
    long active_workers = 0;
    for (long worker = 0; worker < config.concurrency; worker++) {
        long count = worker_request_count(config.requests, config.concurrency, worker);
        slots[worker].worker = worker;
        slots[worker].remaining = count;
        if (count > 0) {
            if (configure_easy(multi, &slots[worker], &config, resolve) != 0) {
                return 1;
            }
            active_workers++;
        }
    }

    int running = 0;

    while (stats.latency_count < config.requests && active_workers > 0) {
        curl_multi_perform(multi, &running);

        int messages = 0;
        CURLMsg *message = NULL;
        while ((message = curl_multi_info_read(multi, &messages)) != NULL) {
            if (message->msg != CURLMSG_DONE) {
                continue;
            }
            Slot *slot = NULL;
            curl_easy_getinfo(message->easy_handle, CURLINFO_PRIVATE, &slot);
            if (slot == NULL) {
                fprintf(stderr, "missing slot for completed transfer\n");
                return 1;
            }
            record_done(slot, message->data.result, &stats);
            curl_multi_remove_handle(multi, slot->easy);
            if (slot->remaining > 0) {
                if (configure_easy(multi, slot, &config, resolve) != 0) {
                    return 1;
                }
            } else {
                curl_easy_cleanup(slot->easy);
                slot->easy = NULL;
                active_workers--;
            }
        }

        if (stats.latency_count >= config.requests || active_workers == 0) {
            break;
        }

        int numfds = 0;
        CURLMcode poll_code = curl_multi_poll(multi, NULL, 0, 1000, &numfds);
        if (poll_code != CURLM_OK) {
            fprintf(stderr, "curl_multi_poll failed: %s\n", curl_multi_strerror(poll_code));
            return 1;
        }
        (void)numfds;
    }

    double total_ms = (double)(now_ns() - total_started) / 1000000.0;
    print_stats(&config, &stats, slots, total_ms);

    for (long i = 0; i < config.concurrency; i++) {
        if (slots[i].easy != NULL) {
            curl_multi_remove_handle(multi, slots[i].easy);
            curl_easy_cleanup(slots[i].easy);
        }
    }
    free(slots);
    free(stats.latencies);
    curl_slist_free_all(resolve);
    curl_multi_cleanup(multi);
    curl_global_cleanup();

    return stats.total_errors == 0 ? 0 : 1;
}
