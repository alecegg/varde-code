/* Expression-level fixtures: Call (call_expression), MemberAccess
   (field_expression, both `.` and `->`), Literal (number/string/char), and
   ControlFlow (if/for/while/do/switch/case/return/break/continue/goto plus the
   ?: ternary). */

int run(struct Node *node, struct Node here) {
    int total = 0;
    int child = node->value;
    int local = here.value;

    if (child > 0) {
        total = child;
    }
    for (int i = 0; i < 10; i++) {
        total = total + i;
    }
    while (total > 100) {
        total = total - 1;
    }
    do {
        total = total + 1;
    } while (total < 5);
    switch (total) {
        case 1:
            break;
        default:
            continue;
    }

    int flag = total ? 1 : 0;
    char c = 'z';
    const char *s = "hello";
    double pi = 3.14;

    printf("%d", total);
    goto done;

done:
    return total;
}
