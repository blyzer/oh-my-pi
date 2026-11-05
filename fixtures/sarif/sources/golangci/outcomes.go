package outcomes

import (
	"fmt"
	"os"
)

func Write(path string) {
	os.Remove(path)
	fmt.Printf("%d\n", path)
}

func helper() {}
