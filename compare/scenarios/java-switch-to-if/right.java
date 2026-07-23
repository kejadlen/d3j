class Router {
    String route(int code) {
        if (code == 1) {
            return "a";
        }
        if (code == 2) {
            return "b";
        }
        return "?";
    }
}
