/* Fixed-size ring buffer used by the sample scheduler. */
#include <stddef.h>

#define CAPACITY 16 // must be a power of two

typedef struct {
    int items[CAPACITY];
    size_t head, tail;
} Queue;

/* Adds a value; returns 0 when the queue is full. */
int queue_push(Queue *q, int value) {
    size_t next = (q->tail + 1) & (CAPACITY - 1);
    if (next == q->head) return 0;
    q->items[q->tail] = value;
    q->tail = next;
    return 1;
}

// Takes the oldest value; returns 0 when the queue is empty.
int queue_pop(Queue *q, int *out) {
    if (q->head == q->tail) return 0;
    *out = q->items[q->head];
    q->head = (q->head + 1) & (CAPACITY - 1);
    return 1;
}
