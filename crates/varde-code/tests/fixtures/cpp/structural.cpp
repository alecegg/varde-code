// Structural fixtures: Function (methods + free functions), Class
// (class_specifier with a base_class_clause -> Extends, plus struct/enum),
// Variable (member + local), Parameter, and Import (#include + using).

#include <string>
#include "helper.h"

using std::string;

namespace app {

class Base {
public:
    virtual void go();
};

class Widget : public Base {
public:
    int count = 0;

    void run(const string& name) {
        int local = count;
        this->count = local;
    }
};

struct Pair {
    int first;
    int second;
};

enum Mode { FAST, SLOW };

int total(int a, int b) {
    return a + b;
}

}  // namespace app
