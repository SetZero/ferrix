/* check.h itself: a failed check names its file, line and expression. */

#include "check.h"

int main(void)
{
	CHECK(1 + 1 == 2);
	/* The expected message in c_programs.rs names this line. */
	CHECK(1 + 1 == 3);
	return t_status;
}
