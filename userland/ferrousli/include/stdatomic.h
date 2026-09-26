#ifndef _STDATOMIC_H
#define _STDATOMIC_H

typedef enum {
	memory_order_relaxed = __ATOMIC_RELAXED,
	memory_order_consume = __ATOMIC_CONSUME,
	memory_order_acquire = __ATOMIC_ACQUIRE,
	memory_order_release = __ATOMIC_RELEASE,
	memory_order_acq_rel = __ATOMIC_ACQ_REL,
	memory_order_seq_cst = __ATOMIC_SEQ_CST
} memory_order;

typedef _Atomic _Bool atomic_bool;
typedef _Atomic char atomic_char;
typedef _Atomic signed char atomic_schar;
typedef _Atomic unsigned char atomic_uchar;
typedef _Atomic short atomic_short;
typedef _Atomic unsigned short atomic_ushort;
typedef _Atomic int atomic_int;
typedef _Atomic unsigned int atomic_uint;
typedef _Atomic long atomic_long;
typedef _Atomic unsigned long atomic_ulong;
typedef _Atomic long long atomic_llong;
typedef _Atomic unsigned long long atomic_ullong;
typedef _Atomic __CHAR16_TYPE__ atomic_char16_t;
typedef _Atomic __CHAR32_TYPE__ atomic_char32_t;
typedef _Atomic __WCHAR_TYPE__ atomic_wchar_t;
typedef _Atomic __INT_LEAST8_TYPE__ atomic_int_least8_t;
typedef _Atomic __UINT_LEAST8_TYPE__ atomic_uint_least8_t;
typedef _Atomic __INT_LEAST16_TYPE__ atomic_int_least16_t;
typedef _Atomic __UINT_LEAST16_TYPE__ atomic_uint_least16_t;
typedef _Atomic __INT_LEAST32_TYPE__ atomic_int_least32_t;
typedef _Atomic __UINT_LEAST32_TYPE__ atomic_uint_least32_t;
typedef _Atomic __INT_LEAST64_TYPE__ atomic_int_least64_t;
typedef _Atomic __UINT_LEAST64_TYPE__ atomic_uint_least64_t;
typedef _Atomic __INT_FAST8_TYPE__ atomic_int_fast8_t;
typedef _Atomic __UINT_FAST8_TYPE__ atomic_uint_fast8_t;
typedef _Atomic __INT_FAST16_TYPE__ atomic_int_fast16_t;
typedef _Atomic __UINT_FAST16_TYPE__ atomic_uint_fast16_t;
typedef _Atomic __INT_FAST32_TYPE__ atomic_int_fast32_t;
typedef _Atomic __UINT_FAST32_TYPE__ atomic_uint_fast32_t;
typedef _Atomic __INT_FAST64_TYPE__ atomic_int_fast64_t;
typedef _Atomic __UINT_FAST64_TYPE__ atomic_uint_fast64_t;
typedef _Atomic __INTPTR_TYPE__ atomic_intptr_t;
typedef _Atomic __UINTPTR_TYPE__ atomic_uintptr_t;
typedef _Atomic __SIZE_TYPE__ atomic_size_t;
typedef _Atomic __PTRDIFF_TYPE__ atomic_ptrdiff_t;
typedef _Atomic __INTMAX_TYPE__ atomic_intmax_t;
typedef _Atomic __UINTMAX_TYPE__ atomic_uintmax_t;

#if defined(__CHAR8_TYPE__)
typedef _Atomic __CHAR8_TYPE__ atomic_char8_t;
#endif

#define ATOMIC_VAR_INIT(value) (value)

#define ATOMIC_BOOL_LOCK_FREE __GCC_ATOMIC_BOOL_LOCK_FREE
#define ATOMIC_CHAR_LOCK_FREE __GCC_ATOMIC_CHAR_LOCK_FREE
#define ATOMIC_CHAR16_T_LOCK_FREE __GCC_ATOMIC_CHAR16_T_LOCK_FREE
#define ATOMIC_CHAR32_T_LOCK_FREE __GCC_ATOMIC_CHAR32_T_LOCK_FREE
#define ATOMIC_WCHAR_T_LOCK_FREE __GCC_ATOMIC_WCHAR_T_LOCK_FREE
#define ATOMIC_SHORT_LOCK_FREE __GCC_ATOMIC_SHORT_LOCK_FREE
#define ATOMIC_INT_LOCK_FREE __GCC_ATOMIC_INT_LOCK_FREE
#define ATOMIC_LONG_LOCK_FREE __GCC_ATOMIC_LONG_LOCK_FREE
#define ATOMIC_LLONG_LOCK_FREE __GCC_ATOMIC_LLONG_LOCK_FREE
#define ATOMIC_POINTER_LOCK_FREE __GCC_ATOMIC_POINTER_LOCK_FREE

#define kill_dependency(value) __extension__ ({ \
	__auto_type __ferrix_dependency = (value); \
	__ferrix_dependency; \
})

#define atomic_thread_fence(order) __atomic_thread_fence(order)
#define atomic_signal_fence(order) __atomic_signal_fence(order)
#define atomic_is_lock_free(object) \
	__atomic_is_lock_free(sizeof(*(object)), (object))

#define atomic_store_explicit(object, desired, order) __extension__ ({ \
	__auto_type __ferrix_store_object = (object); \
	__typeof__((void)0, *__ferrix_store_object) __ferrix_store_value = (desired); \
	__atomic_store(__ferrix_store_object, &__ferrix_store_value, (order)); \
})
#define atomic_store(object, desired) \
	atomic_store_explicit((object), (desired), memory_order_seq_cst)
#define atomic_init(object, desired) \
	atomic_store_explicit((object), (desired), memory_order_relaxed)

#define atomic_load_explicit(object, order) __extension__ ({ \
	__auto_type __ferrix_load_object = (object); \
	__typeof__((void)0, *__ferrix_load_object) __ferrix_load_value; \
	__atomic_load(__ferrix_load_object, &__ferrix_load_value, (order)); \
	__ferrix_load_value; \
})
#define atomic_load(object) \
	atomic_load_explicit((object), memory_order_seq_cst)

#define atomic_exchange_explicit(object, desired, order) __extension__ ({ \
	__auto_type __ferrix_exchange_object = (object); \
	__typeof__((void)0, *__ferrix_exchange_object) __ferrix_exchange_desired = \
	    (desired); \
	__typeof__((void)0, *__ferrix_exchange_object) __ferrix_exchange_old; \
	__atomic_exchange(__ferrix_exchange_object, &__ferrix_exchange_desired, \
	    &__ferrix_exchange_old, (order)); \
	__ferrix_exchange_old; \
})
#define atomic_exchange(object, desired) \
	atomic_exchange_explicit((object), (desired), memory_order_seq_cst)

#define atomic_compare_exchange_strong_explicit(object, expected, desired, \
    success, failure) __extension__ ({ \
	__auto_type __ferrix_compare_object = (object); \
	__typeof__((void)0, *__ferrix_compare_object) __ferrix_compare_desired = \
	    (desired); \
	__atomic_compare_exchange(__ferrix_compare_object, (expected), \
	    &__ferrix_compare_desired, 0, (success), (failure)); \
})
#define atomic_compare_exchange_strong(object, expected, desired) \
	atomic_compare_exchange_strong_explicit((object), (expected), (desired), \
	    memory_order_seq_cst, memory_order_seq_cst)

#define atomic_compare_exchange_weak_explicit(object, expected, desired, \
    success, failure) __extension__ ({ \
	__auto_type __ferrix_compare_object = (object); \
	__typeof__((void)0, *__ferrix_compare_object) __ferrix_compare_desired = \
	    (desired); \
	__atomic_compare_exchange(__ferrix_compare_object, (expected), \
	    &__ferrix_compare_desired, 1, (success), (failure)); \
})
#define atomic_compare_exchange_weak(object, expected, desired) \
	atomic_compare_exchange_weak_explicit((object), (expected), (desired), \
	    memory_order_seq_cst, memory_order_seq_cst)

#define atomic_fetch_add_explicit(object, operand, order) \
	__atomic_fetch_add((object), (operand), (order))
#define atomic_fetch_add(object, operand) \
	atomic_fetch_add_explicit((object), (operand), memory_order_seq_cst)
#define atomic_fetch_sub_explicit(object, operand, order) \
	__atomic_fetch_sub((object), (operand), (order))
#define atomic_fetch_sub(object, operand) \
	atomic_fetch_sub_explicit((object), (operand), memory_order_seq_cst)
#define atomic_fetch_or_explicit(object, operand, order) \
	__atomic_fetch_or((object), (operand), (order))
#define atomic_fetch_or(object, operand) \
	atomic_fetch_or_explicit((object), (operand), memory_order_seq_cst)
#define atomic_fetch_xor_explicit(object, operand, order) \
	__atomic_fetch_xor((object), (operand), (order))
#define atomic_fetch_xor(object, operand) \
	atomic_fetch_xor_explicit((object), (operand), memory_order_seq_cst)
#define atomic_fetch_and_explicit(object, operand, order) \
	__atomic_fetch_and((object), (operand), (order))
#define atomic_fetch_and(object, operand) \
	atomic_fetch_and_explicit((object), (operand), memory_order_seq_cst)

typedef _Atomic struct {
	_Bool __value;
} atomic_flag;

#define ATOMIC_FLAG_INIT { 0 }
#define atomic_flag_test_and_set_explicit(object, order) \
	__atomic_test_and_set((object), (order))
#define atomic_flag_test_and_set(object) \
	atomic_flag_test_and_set_explicit((object), memory_order_seq_cst)
#define atomic_flag_clear_explicit(object, order) \
	__atomic_clear((object), (order))
#define atomic_flag_clear(object) \
	atomic_flag_clear_explicit((object), memory_order_seq_cst)

#if defined(__STDC_VERSION__) && __STDC_VERSION__ > 201710L
#define __STDC_VERSION_STDATOMIC_H__ 202311L
#endif

#endif
