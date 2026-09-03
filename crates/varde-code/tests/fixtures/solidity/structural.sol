// SPDX-License-Identifier: MIT
pragma solidity ^0.8.0;

import "./helper.sol";
import {Token, Vault} from "./tokens.sol";

interface IFace {
    function ping() external returns (uint);
}

library MathLib {
    function add(uint a, uint b) internal pure returns (uint) {
        return a + b;
    }
}

// `is Base, IFace` -> Extends Base + Implements IFace (owned by Wallet).
contract Wallet is Base, IFace {
    uint public count;
    address owner;

    constructor(uint start) {
        count = start;
    }

    function deposit(uint amount) public returns (uint) {
        uint total = count + amount;
        bool ok = true;
        string memory label = "deposit";
        count = total;
        return repo.save(total);
    }
}
