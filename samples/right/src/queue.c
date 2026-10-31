/*
 * Fixed-size ring buffer used by the sample scheduler.
 * Not thread safe: callers hold the scheduler lock.
 */
#include <stddef.h>

#define CAPACITY 16 // a power of two, so masking wraps the index

typedef struct {
	int items[CAPACITY];
	size_t head,   tail;
} Queue;

/* Adds a value. Returns 0 when the queue is full, 1 otherwise. */
int queue_push(Queue *q, int value) {
	size_t next = (q->tail + 1) & (CAPACITY - 1);
	if (next == q->head) return 0;
	q->items[q->tail] = value;
	q->tail = next;
	return 1;
}

// Takes the oldest value. Returns 0 when the queue is empty.
int queue_pop(Queue *q, int *out) {
	if (q->head == q->tail) return 0;
	*out = q->items[q->head];
	q->head = (q->head + 1) & (CAPACITY - 1);
	return 1;
}
