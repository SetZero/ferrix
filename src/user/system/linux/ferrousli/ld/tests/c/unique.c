/* A library defining a symbol with STB_GNU_UNIQUE binding, as GCC gives C++'s
   inline static data -- libstdc++'s std::numpunct<char>::id is one -- and a
   function returning its address, which reaches it through the GOT as the
   program does. */

__asm__(".pushsection .data.unique_value,\"aw\",@progbits\n"
	".globl unique_value\n"
	".type unique_value, @gnu_unique_object\n"
	".p2align 2\n"
	"unique_value: .long 40\n"
	".size unique_value, 4\n"
	".popsection\n");

extern int unique_value;

int *unique_address(void) { return &unique_value; }
