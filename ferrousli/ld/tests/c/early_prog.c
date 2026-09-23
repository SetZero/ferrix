/* `crt1.o` starts this through Ferrousli's __libc_start_main, after the
   loader has run libearly.so's constructor. The page size that constructor
   read before the C library started must be the one main reads after it. */

extern unsigned long getauxval(unsigned long type);
extern unsigned long early_page_size(void);

int main(void)
{
	unsigned long now = getauxval(6);
	if (now == 0)
		return 1;
	if (early_page_size() != now)
		return 2;
	return 42;
}
