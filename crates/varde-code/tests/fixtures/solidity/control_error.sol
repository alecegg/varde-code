// SPDX-License-Identifier: MIT
pragma solidity ^0.8.0;

contract Guarded {
    uint balance;

    error Denied(uint code);

    function g() external returns (uint) {
        return 1;
    }

    function withdraw(uint amount) public returns (uint) {
        require(amount > 0, "zero");
        assert(balance >= 0);
        if (amount > balance) {
            revert("insufficient");
        }
        if (amount == 42) {
            revert Denied(42);
        }
        for (uint i = 0; i < amount; i++) {
            balance = balance - 1;
        }
        while (balance > 0) {
            balance--;
        }
        try this.g() returns (uint r) {
            return r;
        } catch {
            return 0;
        }
        return balance;
    }
}
