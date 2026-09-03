// Expression-level fixtures: Call (call_expression + `new` constructor),
// MemberAccess (field_expression), Literal (number/string/char/bool),
// ControlFlow (if/for/while/switch/return/try + ternary), and the exception
// pair Catch (catch_clause) / Throw (throw_statement).

#include <stdexcept>

class Runner {
public:
    void execute(Widget* target) {
        int value = target->count;
        Base* made = new Base();

        try {
            made->go();
            throw std::runtime_error("boom");
        } catch (const std::exception& e) {
            handle(e);
        }

        if (value > 0) {
            return;
        }
        for (int i = 0; i < 3; i++) {
            value = value + i;
        }
        while (value) {
            value = value - 1;
        }
        switch (value) {
            case 1:
                break;
            default:
                continue;
        }

        int flag = value ? 1 : 0;
        bool ok = true;
        char c = 'q';
        const char* s = "hi";
        double d = 2.5;
    }
};
