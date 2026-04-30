document.addEventListener('DOMContentLoaded', function() {
    var nav         = document.getElementById("-nav");
    var nav_toggle  = document.getElementById("-nav-toggle");
    var content     = document.getElementById("-content");

    var link_top    = document.getElementById("-top-link");
    var top         = document.getElementById("-top");
    var link_about  = document.getElementById("-about-link");
    var about       = document.getElementById("-about");
    var link_exp    = document.getElementById("-exp-link");
    var experience  = document.getElementById("-experience");
    var link_tech   = document.getElementById("-tech-link");
    var technology  = document.getElementById("-technology");
    var link_proj   = document.getElementById("-proj-link");
    var projects    = document.getElementById("-projects");
    var link_ed     = document.getElementById("-ed-link");
    var education   = document.getElementById("-education");
    var side_links = [
        [link_top, top],
        [link_about, about],
        [link_exp, experience],
        [link_tech, technology],
        [link_proj, projects],
        [link_ed, education],
    ];

    function width() {
        return window.innerWidth
            || document.documentElement.clientWidth
            || document.body.clientWidth;
    }

    var isNavOpen = false;
    var navThresh = 1000;
    function navOpen() {
        nav.classList.add("nav-open");
        content.classList.add("nav-open");
        content.classList.add("slide-open");
        if (width() < navThresh) {
            content.classList.remove("offset");
        } else {
            content.classList.add("offset");
        }
        isNavOpen = true;
    }
    function navClose() {
        nav.classList.remove("nav-open");
        content.classList.remove("nav-open");
        content.classList.remove("offset");
        content.classList.remove("slide-open");
        isNavOpen = false;
    }
    nav_toggle.addEventListener('click', function() {
        if (nav.classList.contains("nav-open")) {
            navClose();
        } else {
            navOpen();
        }
    });

    side_links.forEach(function(links){
        links[1].scrollTop = 120;
        links[0].addEventListener("click", function() {
            if (width() < navThresh) {
                navClose();
            }
            setTimeout(function() {
              links[1].scrollIntoView();
            }, 110);
        });
    });

    setTimeout(function scrollToHash() {
        var hash = window.location.hash;
        if (hash) {
            switch (hash) {
                case "#top":
                    top.scrollIntoView();
                    break;
                case "#about":
                    about.scrollIntoView();
                    break;
                case "#projects":
                    projects.scrollIntoView();
                    break;
                case "#technology":
                    technology.scrollIntoView();
                    break;
                case "#experience":
                    experience.scrollIntoView();
                    break;
                case "#education":
                    education.scrollIntoView();
                    break;
                default:
                    break;
            }
        }
    }, 200);

    function handleResize() {
        var _width = width();
        if (_width < navThresh && isNavOpen) {
            navClose();
        } else if (_width > navThresh && !isNavOpen) {
            navOpen();
        }
    }
    // Handle initial page sizing
    setTimeout(handleResize, 200);
    window.addEventListener("resize", handleResize);

});

