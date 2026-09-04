public class OrderController {
    private OrderService svc;
    public int handle() { return svc.placeOrder(); }
}
