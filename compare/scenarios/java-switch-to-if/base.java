class Router {
    String route(int code) {
        switch (code) {
            case 1:
                return "a";
            case 2:
                return "b";
            default:
                return "?";
        }
    }
}
